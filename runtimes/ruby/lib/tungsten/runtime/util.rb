# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

require "openssl"
require "securerandom"
require "digest"

module Tungsten
  module Runtime
    # Helpers shared by the pipeline: field paths, redaction, secret
    # scrubbing, digests.
    module Util
      # The longest string kept in received_value, in UTF-16 code units.
      MAX_RECEIVED_CHARS = 200
      # The deepest JSON nesting accepted in arguments.
      MAX_ARG_DEPTH = 256
      # The largest integer a JavaScript number holds exactly.
      MAX_SAFE_INTEGER = 9_007_199_254_740_991

      SENSITIVE_WORDS = %w[secret password passwd passphrase token apikey api-key api_key privatekey
                           private-key private_key credential authorization cookie session].freeze

      module_function

      def sha256_hex(data) = Digest::SHA256.hexdigest(data.b)

      def hmac_sha256(key, message) = OpenSSL::HMAC.digest("SHA256", key.b, message.b)

      def base64url(data) = Base64.urlsafe_encode64(data, padding: false)

      def timing_safe_equal?(a, b)
        return false unless a.bytesize == b.bytesize

        OpenSSL.fixed_length_secure_compare(a, b)
      end

      # A random UUID version 4.
      def uuid_v4 = SecureRandom.uuid

      # Split a field path (a.b, items[0].id, items.0.id).
      def split_path(path)
        return [] if path.nil? || path == "" || path == "."

        path.to_s.gsub(/\[(\d+)\]/) { ".#{Regexp.last_match(1)}" }.split(".").reject(&:empty?)
      end

      def index?(segment) = segment.is_a?(String) && segment.match?(/\A\d+\z/)

      # The value at a path of segments, or UNSET.
      def get_path(value, segments)
        return UNSET if UNSET.equal?(value)

        segments.each do |seg|
          case value
          when Array
            key = seg.is_a?(Integer) ? seg : (index?(seg) ? seg.to_i : nil)
            return UNSET if key.nil? || key >= value.length

            value = value[key]
          when Hash
            key = seg.to_s
            return UNSET unless value.key?(key)

            value = value[key]
          else
            return UNSET
          end
        end
        value
      end

      def get_path_str(value, path) = get_path(value, split_path(path))

      # Whether a name very likely holds a secret.
      def looks_sensitive?(name)
        lower = name.to_s.downcase
        SENSITIVE_WORDS.any? { |w| lower.include?(w) }
      end

      def redact_sensitive_keys(value, depth = 0)
        return nil if depth > 32

        case value
        when Array then value.map { |v| redact_sensitive_keys(v, depth + 1) }
        when Hash
          value.each_with_object({}) do |(k, v), out|
            out[k] = looks_sensitive?(k) ? REDACTED : redact_sensitive_keys(v, depth + 1)
          end
        else value
        end
      end

      def redact_at(value, segments)
        return REDACTED if segments.empty?

        head = segments.first
        rest = segments.drop(1)
        case value
        when Array then value.map { |v| redact_at(v, segments) }
        when Hash
          return value unless value.key?(head)

          out = value.dup
          out[head] = redact_at(value[head], rest)
          out
        else value
        end
      end

      # A copy of value with every listed field path redacted; arrays on the
      # way fan out.
      def redact_paths(value, paths)
        out = Json.copy(value)
        paths.each do |p|
          segs = split_path(p)
          out = redact_at(out, segs) unless segs.empty?
        end
        out
      end

      def cut(text)
        return text if Json.utf16_length(text) <= MAX_RECEIVED_CHARS

        "#{Json.take_units(text, MAX_RECEIVED_CHARS - 1)}\u2026"
      end

      # A value made safe for received_value: redacted when sensitive,
      # strings cut to 200 UTF-16 code units, containers with sensitive
      # members redacted and cut as JSON text when long.
      def envelope_value(value, sensitive)
        return REDACTED if sensitive
        return nil if UNSET.equal?(value)

        case value
        when String then cut(value)
        when Binary then "<#{value.bytesize} bytes>"
        when Array, Hash
          bin = Json.binary_of(value)
          return "<#{bin.bytesize} bytes>" unless UNSET.equal?(bin)

          text = Json.text(redact_sensitive_keys(value))
          return cut(text) if Json.utf16_length(text) > MAX_RECEIVED_CHARS

          Json.try_parse(text).then { |v| UNSET.equal?(v) ? nil : v }
        else value
        end
      end

      # Secret values to scrub from texts, in insertion order.
      class Secrets
        attr_reader :items

        def initialize(items = [])
          @items = items.dup
        end

        def <<(secret)
          return self if secret.nil?

          text = secret.to_s
          @items << text unless text.empty? || @items.include?(text)
          self
        end

        def empty? = @items.empty?
        def dup = Secrets.new(@items)
      end

      # Every occurrence of the secrets (four characters or more) replaced.
      def scrub_text(text, secrets)
        return text if text.nil? || secrets.nil?

        secrets.items.each do |secret|
          text = text.gsub(secret, REDACTED) if secret.length >= 4 && text.include?(secret)
        end
        text
      end

      def scrub_value(value, secrets)
        case value
        when String then scrub_text(value, secrets)
        when Array, Hash
          parsed = Json.try_parse(scrub_text(Json.text(value), secrets))
          UNSET.equal?(parsed) ? REDACTED : parsed
        else value
        end
      end

      # The envelope with the secrets scrubbed from its text members: a last
      # line of defence.
      def scrub_diagnostic(d, secrets)
        return d if secrets.nil? || secrets.empty?

        d.with(
          code: scrub_text(d.code, secrets), failed_parameter: scrub_text(d.failed_parameter, secrets),
          received_value: scrub_value(d.received_value, secrets), expected: scrub_text(d.expected, secrets),
          remediation: scrub_text(d.remediation, secrets), next_action: scrub_text(d.next_action, secrets),
          request_id: scrub_text(d.request_id, secrets)
        )
      end

      # The Accept of a stream call: the caller's media ranges in order
      # without repeats, text/event-stream first when they do not name it.
      def merge_accept(value)
        ranges = []
        value.to_s.split(",").each do |part|
          r = part.strip
          ranges << r unless r.empty? || ranges.include?(r)
        end
        names = ranges.any? { |r| r.split(";").first.to_s.strip.casecmp?("text/event-stream") }
        ranges.unshift("text/event-stream") unless names
        ranges.join(", ")
      end

      # Whether a value nests deeper than limit.
      def nests_deeper?(value, limit)
        case value
        when Array then limit.zero? || value.any? { |v| nests_deeper?(v, limit - 1) }
        when Hash then limit.zero? || value.each_value.any? { |v| nests_deeper?(v, limit - 1) }
        else false
        end
      end

      # A finite number within [min, max], else the fallback.
      def bounded(value, fallback, min, max)
        return fallback unless value.is_a?(Numeric)
        return fallback if value.is_a?(Float) && (value.nan? || value.infinite?)

        value.clamp(min, max)
      end

      # A number as JSON writes it, integral floats as integers.
      def as_number(value)
        value.is_a?(Float) && value == value.truncate && value.abs < MAX_SAFE_INTEGER ? value.to_i : value
      end

      # A string literal as JSON.stringify writes it.
      def js_string(text) = Json.quote(text)

      # Remediation JSON, cut to 80 characters.
      def short_json(value)
        text = Json.text(value)
        text.length > 80 ? "#{text[0, 77]}..." : text
      end

      # " with name value, ..." for remediation text.
      def with_arguments(args)
        return "" unless args.is_a?(Hash) && !args.empty?

        " with #{args.map { |k, v| "#{k} #{short_json(v)}" }.join(', ')}"
      end
    end
  end
end
