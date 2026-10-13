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
        @buffer = +""
        @started = false
        reset_event
        @id = nil
        @retry_ms = nil
        @exceeded = false
      end

      def exceeded? = @exceeded

      # Feed decoded text; the events it completes. A CR at the end is held
      # back until the next text shows whether an LF follows.
      def push(chunk)
        return [] if @exceeded

        text = @buffer + chunk
        @buffer = +""
        if !@started && !text.empty?
          @started = true
          text = text.delete_prefix("\uFEFF")
        end
        out = []
        bytes = text.b
        start = 0
        while (i = bytes.byteindex(/[\r\n]/n, start))
          cr = bytes.getbyte(i) == 13
          if cr && i + 1 == bytes.bytesize
            @buffer = utf8(bytes.byteslice(start..))
            return out
          end
          line(utf8(bytes.byteslice(start, i - start)), out)
          start = cr && bytes.getbyte(i + 1) == 10 ? i + 2 : i + 1
          return out if @exceeded
        end
        @buffer = utf8(bytes.byteslice(start..) || "")
        if @event_bytes + @buffer.bytesize > @max
          @exceeded = true
          @buffer = +""
        end
        out
      end

      # The stream ended: a held-back CR ends its line, the rest is
      # discarded.
      def finish
        out = []
        line(@buffer.delete_suffix("\r"), out) if !@exceeded && @buffer.end_with?("\r")
        @buffer = +""
        reset_event
        out
      end

      private

      def utf8(bytes) = bytes.dup.force_encoding(Encoding::UTF_8)

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
