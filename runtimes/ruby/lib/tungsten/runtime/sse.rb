# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # A server-sent events parser (WHATWG HTML 9.2): lines end with CRLF, LF
    # or CR; a line is a comment (":"), a field (event, data, id, retry; the
    # value loses one leading space) or the blank line that dispatches the
    # event. Data lines are joined with LF; an event without data lines is
    # not dispatched; an event still open when the stream ends is
    # discarded. An event may hold at most `max` UTF-8 bytes (its field
    # lines, each with a one-byte terminator, plus the line being read);
    # past it the parser stops (`exceeded?`).
    class SseParser
      # One dispatched event; `id` and `retry_ms` persist across events
      # (nil before any).
      Event = Data.define(:event, :data, :id, :retry_ms)

      def initialize(max)
        @max = max
        @line = String.new(encoding: Encoding::BINARY)
        @pending_cr = false
        @started = false
        reset_event
        @id = nil
        @retry_ms = nil
        @exceeded = false
      end

      def exceeded? = @exceeded

      # Feed decoded text; the events it completes. A CR at the end is held
      # back until the next text shows whether an LF follows. Each fragment
      # is scanned once and joined to the line being read only by byte
      # appends, so a long line delivered in small pieces costs linear time.
      def push(chunk)
        return [] if @exceeded

        if !@started && !chunk.empty?
          @started = true
          chunk = chunk.delete_prefix("\uFEFF")
        end
        out = []
        start = 0
        bytes = chunk.b
        if @pending_cr
          return out if bytes.empty?

          @pending_cr = false
          complete(out)
          return out if @exceeded

          start = 1 if bytes.getbyte(0) == 10
        end
        size = bytes.bytesize
        while (i = bytes.byteindex(/[\r\n]/n, start))
          @line << bytes.byteslice(start, i - start)
          cr = bytes.getbyte(i) == 13
          if cr && i + 1 == size
            @pending_cr = true
            return out
          end
          complete(out)
          return out if @exceeded

          start = cr && bytes.getbyte(i + 1) == 10 ? i + 2 : i + 1
        end
        @line << bytes.byteslice(start, size - start)
        if @event_bytes + @line.bytesize > @max
          @exceeded = true
          @line.clear
        end
        out
      end

      # The stream ended: a held-back CR ends its line, the rest is
      # discarded.
      def finish
        out = []
        complete(out) if !@exceeded && @pending_cr
        @pending_cr = false
        @line.clear
        reset_event
        out
      end

      private

      def utf8(bytes) = bytes.dup.force_encoding(Encoding::UTF_8)

      # The line read so far ends: it is parsed and the buffer is emptied.
      def complete(out)
        text = utf8(@line)
        @line.clear
        line(text, out)
      end

      def reset_event
        @event = ""
        @data = +""
        @has_data = false
        @event_bytes = 0
      end

      def line(text, out)
        if text.empty?
          if @has_data
            out << Event.new(event: @event.empty? ? "message" : @event, data: @data.delete_suffix("\n"), id: @id, retry_ms: @retry_ms)
          end
          reset_event
          return
        end
        return if text.start_with?(":")

        @event_bytes += text.bytesize + 1
        if @event_bytes > @max
          @exceeded = true
          return
        end
        name, value = text.split(":", 2)
        value = value.to_s.delete_prefix(" ")
        case name
        when "event" then @event = value
        when "data"
          @data << value << "\n"
          @has_data = true
        when "id" then @id = value unless value.include?("\u0000")
        when "retry"
          @retry_ms = value.to_i if value.match?(/\A\d+\z/) && value.to_i <= Util::MAX_SAFE_INTEGER
        end
      end
    end

    # Joins UTF-8 sequences split across chunks; invalid bytes become U+FFFD.
    class Utf8Decoder
      def initialize
        @pending = "".b
      end

      def decode(data)
        buf = @pending + data.b
        cut = buf.bytesize
        [3, buf.bytesize].min.times do |back|
          i = back + 1
          b = buf.getbyte(buf.bytesize - i)
          next if (b & 0xC0) == 0x80

          if b.between?(0xC2, 0xF4)
            need = if b >= 0xF0 then 4
                   elsif b >= 0xE0 then 3
                   else 2
                   end
            cut = buf.bytesize - i if need > i
          end
          break
        end
        @pending = buf.byteslice(cut..) || "".b
        buf.byteslice(0, cut).force_encoding(Encoding::UTF_8).scrub("\uFFFD")
      end

      def finish
        return "" if @pending.empty?

        @pending = "".b
        "\uFFFD"
      end
    end
  end
end
