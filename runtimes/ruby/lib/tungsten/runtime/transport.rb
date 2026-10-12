# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

require "net/http"
require "uri"
require "timeout"

module Tungsten
  module Runtime
    # One HTTP attempt over Net::HTTP: HTTP/1.1, a new connection per
    # attempt, no redirects (the client core follows same-origin redirects
    # of reads itself), no automatic retries (`max_retries = 0`), no
    # compression (`Accept-Encoding` is not sent), an explicit `Accept` and
    # `User-Agent`, `Content-Length: 0` on a bodiless POST, PUT or PATCH,
    # and an absolute deadline per attempt, including all response headers:
    # the open, read and write timeouts also use the time left on a
    # monotonic clock. An event stream's body is handed back
    # unread; each read of it waits at most the idle timeout (else the
    # attempt timeout), and closing it closes the connection.
    module Transport
      # Internal control flow must escape broad StandardError handlers in
      # Net::HTTP and the call pipeline. Only the deadline owner catches it.
      class DeadlineExceeded < Exception; end # rubocop:disable Lint/InheritException
      # What one attempt gave. `kind` is :response, :streaming, :not_sent
      # (the request never left: DNS, connect, TLS), :lost (the connection
      # failed after the request was written) or :timeout. A :response has
      # `status`, `headers` (lower-cased names) and `body` (a binary String,
      # nil with `body_failure` "timeout" or "broken" when reading it
      # failed); a :streaming one has `stream`, an EventBody.
      Outcome = Data.define(:kind, :status, :headers, :body, :body_failure, :stream, :detail) do
        def initialize(kind:, status: 0, headers: {}, body: nil, body_failure: nil, stream: nil, detail: nil) = super
      end

      # The unread body of an event stream.
      class EventBody
        def initialize(http, chunks, idle_s)
          @http = http
          @chunks = chunks
          @idle_s = idle_s
          @closed = false
          @lock = Mutex.new
        end

        # The next chunk: [:chunk, bytes], [:end], [:timeout] or [:lost].
        def read
          return [:lost] if closed?

          @http.read_timeout = @idle_s
          item = @chunks.next
          item.first == :chunk ? item : [:end]
        rescue StopIteration
          [:end]
        rescue Net::ReadTimeout, IO::TimeoutError
          [:timeout]
        rescue StandardError
          [:lost]
        end

        def closed? = @lock.synchronize { @closed }

        # Close the connection; safe from any thread, more than once.
        def close
          first = @lock.synchronize do
            was = @closed
            @closed = true
            !was
          end
          return unless first

          begin
            @http.finish if @http.started?
          rescue StandardError
            nil
          end
        end
      end

      ERRNO_CODES = %w[ECONNREFUSED ECONNRESET ECONNABORTED ENOTCONN EPIPE ETIMEDOUT EADDRNOTAVAIL EHOSTUNREACH
                       ENETUNREACH].freeze

      module_function

      def monotonic_ms = Process.clock_gettime(Process::CLOCK_MONOTONIC, :millisecond)

      # Short, secret-free codes of a transport error.
      def error_codes(error)
        codes = []
        name = error.class.name.to_s.split("::").last
        codes << name if error.is_a?(SystemCallError) && ERRNO_CODES.include?(name)
        codes << "ENOTFOUND" if error.is_a?(SocketError) && !error.is_a?(SystemCallError)
        if defined?(OpenSSL::SSL::SSLError) && error.is_a?(OpenSSL::SSL::SSLError)
          codes << (error.message.include?("certificate verify failed") ? "CERT_VERIFY_FAILED" : "TLS_ERROR")
        end
        codes << "CONNECTION_CLOSED" if error.is_a?(EOFError) || error.message.to_s.include?("end of file")
        codes.uniq
      end

      def detail(error, sent)
        codes = error_codes(error)
        return codes.join(", ") unless codes.empty?

        sent ? "network error" : "connect error"
      end

      # Send once. Never raises: every outcome is an Outcome.
      #
      # @param url [String]
      # @param method [String]
      # @param headers [Array<Array(String, String)>] in order
      # @param body [Serialize::Encoded, nil]
      # @param timeout_ms [Integer] the attempt's deadline
      # @param stream [Boolean] hand a 2xx text/event-stream answer back unread
      # @param idle_ms [Integer, nil] the silence a stream read waits for (default: timeout_ms)
      def attempt(url:, method:, headers:, body:, timeout_ms:, stream: false, idle_ms: nil)
        http = nil
        handed_off = false
        Timeout.timeout(timeout_ms / 1000.0, DeadlineExceeded) do
          uri = URI.parse(url)
          deadline = monotonic_ms + timeout_ms
          left = -> { [(deadline - monotonic_ms) / 1000.0, 0.001].max }
          http = Net::HTTP.new(uri.hostname, uri.port)
          http.use_ssl = uri.scheme == "https"
          http.max_retries = 0
          http.open_timeout = left.call
          http.read_timeout = left.call
          http.write_timeout = left.call
          http.keep_alive_timeout = 0
          begin
            http.start
          rescue Net::OpenTimeout, IO::TimeoutError, DeadlineExceeded
            return Outcome.new(kind: :timeout)
          rescue StandardError => e
            return Outcome.new(kind: :not_sent, detail: detail(e, false))
          end
          request = build(uri, method, headers, body)
          return Outcome.new(kind: :not_sent, detail: request) if request.is_a?(String)

          result = exchange(http, request, stream, left, idle_ms ? idle_ms / 1000.0 : nil, timeout_ms / 1000.0)
          handed_off = result.kind == :streaming
          result
        end
      rescue DeadlineExceeded
        handed_off = false
        Outcome.new(kind: :timeout)
      rescue URI::Error, ArgumentError
        Outcome.new(kind: :not_sent, detail: "INVALID_REQUEST")
      ensure
        close(http) if http && (!handed_off || $!)
      end

      def build(uri, method, headers, body)
        has_body = body && !body.empty
        # Without a body the request permits none: Net::HTTP would send an
        # empty one with a form Content-Type otherwise.
        request = Net::HTTPGenericRequest.new(method, has_body ? true : false, method != "HEAD", uri.request_uri, nil)
        headers.each { |name, value| request[name] = Serialize.header_bytes(value) }
        if has_body
          if body.parts
            data, type = Serialize.multipart_body(body.parts)
            request["Content-Type"] = type
            request.body = data
          else
            request.body = body.bytes
          end
        elsif %w[POST PUT PATCH].include?(method)
          request["Content-Length"] = "0"
        end
        request.delete("accept-encoding")
        request
      rescue ArgumentError
        "INVALID_REQUEST"
      end

      def response_headers(response)
        out = {}
        response.each_name do |name|
          values = response.get_fields(name) || []
          out[name.downcase] = name.casecmp?("set-cookie") ? values.last.to_s : values.join(", ")
        end
        out
      end

      def event_stream?(headers)
        headers.fetch("content-type", "").split(";").first.to_s.strip.casecmp?("text/event-stream")
      end

      def exchange(http, request, stream, left, idle_s, timeout_s)
        chunks = Enumerator.new do |y|
          http.request(request) do |response|
            y << [:response, response]
            response.read_body { |chunk| y << [:chunk, chunk] }
          end
          y << [:end]
        end
        begin
          first = chunks.next
        rescue Net::ReadTimeout, Net::WriteTimeout, IO::TimeoutError, DeadlineExceeded
          close(http)
          return Outcome.new(kind: :timeout)
        rescue StandardError => e
          close(http)
          return Outcome.new(kind: :lost, detail: detail(e, true))
        end
        response = first[1]
        status = response.code.to_i
        headers = response_headers(response)
        if stream && status.between?(200, 299) && event_stream?(headers)
          return Outcome.new(kind: :streaming, status: status, headers: headers,
                             stream: EventBody.new(http, chunks, idle_s || timeout_s))
        end
        body = +"".b
        failure = nil
        loop do
          remaining = left.call
          if remaining <= 0.001
            failure = "timeout"
            break
          end
          http.read_timeout = remaining
          item = chunks.next
          break unless item.first == :chunk

          body << item[1].b
        rescue StopIteration
          break
        rescue Net::ReadTimeout, IO::TimeoutError, DeadlineExceeded
          failure = "timeout"
          break
        rescue StandardError
          failure = "broken"
          break
        end
        close(http)
        Outcome.new(kind: :response, status: status, headers: headers, body: failure ? nil : body, body_failure: failure)
      end

      def close(http)
        http.finish if http.started?
      rescue StandardError
        nil
      end

      # ------------------------------------------------------ retry-after

      MONTHS = %w[jan feb mar apr may jun jul aug sep oct nov dec].freeze
      MONTHS_FULL = %w[january february march april may june july august september october november december].freeze

      def days_from_civil(year, month, day)
        y = month <= 2 ? year - 1 : year
        era = (y >= 0 ? y : y - 399) / 400
        yoe = y - (era * 400)
        mp = (month + 9) % 12
        doy = (((153 * mp) + 2) / 5) + day - 1
        doe = (yoe * 365) + (yoe / 4) - (yoe / 100) + doy
        (era * 146_097) + doe - 719_468
      end

      def month_number(token)
        lower = token.downcase
        return nil if lower.length < 3

        i = MONTHS.index(lower[0, 3])
        return nil unless i && (lower.length == 3 || lower == MONTHS_FULL[i] || lower == "sept")

        i + 1
      end

      def digits?(text) = text.match?(/\A\d+\z/)

      def parse_clock(token)
        parts = token.split(":", -1)
        return nil unless parts.length.between?(2, 3) && digits?(parts[0]) && digits?(parts[1])

        hour = parts[0].to_i
        minute = parts[1].to_i
        return nil unless hour < 24 && minute < 60

        second = 0.0
        if parts.length == 3
          second = Float(parts[2], exception: false)
          return nil if second.nil? || second.negative? || second >= 61
        end
        ((((hour * 3600) + (minute * 60)) * 1000) + ((second * 1000) + 0.5).floor)
      end

      def parse_zone(token)
        return 0 if %w[GMT UTC UT Z].include?(token.upcase)

        m = token.match(/\A([+-])(\d\d):?(\d\d)\z/)
        return nil unless m

        (m[1] == "-" ? -1 : 1) * ((m[2].to_i * 60) + m[3].to_i) * 60_000
      end

      def parse_iso_date(text)
        date, rest = text.split(/[T ]/, 2)
        fields = date.split("-", -1)
        return nil unless fields.length == 3 && fields.all? { |f| digits?(f) }

        year, month, day = fields.map(&:to_i)
        return nil unless month.between?(1, 12) && day.between?(1, 31) && year.between?(0, 9999)

        clock = 0
        zone = 0
        if rest
          split = rest.index(/[Zz+-]/) || rest.length
          clock = parse_clock(rest[0, split])
          return nil unless clock

          if split < rest.length
            zone = parse_zone(rest[split..])
            return nil unless zone
          end
        end
        (days_from_civil(year, month, day) * 86_400_000) + clock - zone
      end

      # An HTTP date (IMF-fixdate, RFC 850, asctime) or an ISO 8601 timestamp
      # as epoch milliseconds, or nil.
      def parse_http_date(text)
        text = text.strip
        return parse_iso_date(text) if text.length >= 8 && text[0].match?(/\d/) && text[4] == "-"

        day = month = year = nil
        clock = 0
        zone = 0
        text.split(/[ ,]+/).reject(&:empty?).each do |token|
          if token.include?(":")
            clock = parse_clock(token)
            return nil unless clock
          elsif token.include?("-") && token[0].match?(/\d/)
            parts = token.split("-")
            return nil if parts.length < 3 || !digits?(parts[0]) || !digits?(parts[2])

            month = month_number(parts[1])
            return nil unless month

            day = parts[0].to_i
            year = parts[2].to_i
            year += 2000 if year < 50
            year += 1900 if year.between?(50, 99)
          elsif digits?(token)
            if day.nil? && token.length <= 2
              day = token.to_i
            else
              year = token.to_i
            end
          elsif (m = month_number(token))
            month = m
          elsif (z = parse_zone(token))
            zone = z
          elsif !token.match?(/\A[A-Za-z.]+\z/)
            return nil
          end
        end
        return nil unless day && month && year && day.between?(1, 31) && year.between?(0, 9999)

        (days_from_civil(year, month, day) * 86_400_000) + clock - zone
      end

      # Retry-After in milliseconds (delta seconds or an HTTP date), or nil.
      def parse_retry_after(value, now_ms)
        return nil if value.nil?

        trimmed = value.strip
        return [trimmed.to_i * 1000, 2**62].min if digits?(trimmed)

        date = parse_http_date(trimmed)
        date && [date - now_ms, 0].max
      end

      # The next page URL of a Link header (rel="next"), or nil.
      def next_link(header)
        return nil if header.nil? || header.empty?

        parts = header.split(/,(?=\s*<)/)
        parts.each do |part|
          m = part.match(/\A\s*<([^>]*)>(.*)\z/m)
          next unless m
          return m[1] if m[2].match?(/;\s*rel\s*=\s*"?([^";]*\s)?next(\s[^";]*)?"?\s*(;|\z)/i)
        end
        nil
      end
    end
  end
end
