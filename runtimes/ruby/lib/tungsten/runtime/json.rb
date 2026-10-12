# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

require "json"
require "base64"

module Tungsten
  module Runtime
    # The marker of an absent value: an optional argument or member that was
    # not given, a path that leads nowhere. It is neither `nil` (JSON null)
    # nor any JSON value.
    UNSET = Object.new
    def UNSET.inspect = "Tungsten::Runtime::UNSET"
    def UNSET.to_s = "UNSET"
    UNSET.freeze

    # Binary data: the bytes of a bytes or text body, a multipart file part
    # (with its optional file name and media type) or a binary response.
    # `inspect` never shows the bytes.
    Binary = Data.define(:data, :filename, :content_type) do
      def initialize(data:, filename: nil, content_type: nil)
        bytes = data.to_s.dup.force_encoding(Encoding::BINARY).freeze
        super(data: bytes, filename: filename&.dup&.freeze, content_type: content_type&.dup&.freeze)
      end

      def inspect = "#<Tungsten::Runtime::Binary #{data.bytesize} bytes>"
      alias_method :to_s, :inspect

      def bytesize = data.bytesize
    end

    # JSON values as the runtime handles them: `nil`, `true`, `false`,
    # `String` (UTF-8), `Integer`, `Float`, `Array`, `Hash` (string keys, in
    # member order) and `Binary`. A number without a fractional part and
    # below 2^53 in magnitude is an `Integer`, as every tungsten runtime
    # reads it; numbers are finite.
    module Json
      # 2^53: integers below it in magnitude are exact in every runtime.
      MAX_SAFE = 9_007_199_254_740_992
      INT64_MIN = -(2**63)
      INT64_MAX = (2**63) - 1
      # Bound recursion before parsing or traversing caller-controlled values.
      MAX_DEPTH = 512

      # Raised when a text is not one JSON document.
      class ParseError < StandardError; end

      module_function

      # Parse one JSON document (invalid UTF-8 replaced, a leading byte order
      # mark dropped). Raises ParseError.
      def parse(text)
        source = text.to_s.dup.force_encoding(Encoding::UTF_8)
        source = source.scrub("\uFFFD") unless source.valid_encoding?
        source = source.delete_prefix("\uFEFF")
        normalize(JSON.parse(source, max_nesting: MAX_DEPTH, allow_nan: false, create_additions: false))
      rescue JSON::ParserError, EncodingError, ArgumentError => e
        raise ParseError, e.message
      end

      # Parse, or UNSET when the text is not JSON.
      def try_parse(text)
        parse(text)
      rescue ParseError
        UNSET
      end

      # A parsed value with the number rules applied.
      def normalize(value, depth = 0)
        check_depth(depth)
        case value
        when Hash then value.each_with_object({}) { |(k, v), out| out[k] = normalize(v, depth + 1) }
        when Array then value.map { |v| normalize(v, depth + 1) }
        when Float then number(value)
        when Integer then value.between?(INT64_MIN, INT64_MAX) ? value : value.to_f
        else value
        end
      end

      # A number: an integral float below 2^53 becomes an Integer.
      def number(value)
        return value unless value.is_a?(Float)
        return value if value.nan? || value.infinite?

        value == value.truncate && value.abs < MAX_SAFE ? value.to_i : value
      end

      # Convert a Ruby value to a JSON value: symbols to strings, hash keys to
      # strings, models (anything that responds to `to_h` and includes
      # `Tungsten::Runtime::Model`) to their wire form; `UNSET` members and
      # items are dropped. Raises ArgumentError for a value that has no JSON
      # form (an object, a non-finite number).
      def from_ruby(value, depth = 0)
        check_depth(depth)

        case value
        when nil, true, false, Binary then value
        when String then value.encoding == Encoding::BINARY ? Binary.new(data: value) : utf8(value).dup
        when Symbol then value.to_s
        when Integer then value.between?(INT64_MIN, INT64_MAX) ? value : value.to_f
        when Float
          raise ArgumentError, "a non-finite number is not JSON" if value.nan? || value.infinite?

          number(value)
        when Hash
          value.each_with_object({}) do |(k, v), out|
            next if UNSET.equal?(v)

            out[k.to_s.dup] = from_ruby(v, depth + 1)
          end
        when Array then value.reject { |v| UNSET.equal?(v) }.map { |v| from_ruby(v, depth + 1) }
        when Model then from_ruby(value.to_h, depth + 1)
        else
          raise ArgumentError, "#{value.class} has no JSON form"
        end
      end

      def utf8(text)
        return text if text.encoding == Encoding::UTF_8 && text.valid_encoding?

        text.dup.force_encoding(Encoding::UTF_8).scrub("\uFFFD")
      end

      # JSON.stringify(value): members in order, no whitespace, numbers as
      # JavaScript writes them; a Binary is its tagged form.
      def text(value)
        out = +""
        write(out, value, false)
        out
      end

      # The canonical JSON every tungsten runtime digests (confirmation
      # tokens, automatic idempotency keys, content hashes): members sorted
      # by the UTF-16 code units of their names, binary values as
      # {"$bytes": <base64>} or {"$file": {"bytes", "name", "type"}}.
      def canonical(value)
        out = +""
        write(out, value, true)
        out
      end

      def write(out, value, canonical, depth = 0)
        check_depth(depth)
        case value
        when nil then out << "null"
        when true then out << "true"
        when false then out << "false"
        when Integer, Float then out << number_text(value)
        when String then out << quote(value)
        when Array
          out << "["
          value.each_with_index do |item, i|
            out << "," if i.positive?
            write(out, item, canonical, depth + 1)
          end
          out << "]"
        when Hash
          keys = value.keys.map(&:to_s)
          keys = keys.sort_by { |k| k.encode(Encoding::UTF_16BE).bytes } if canonical
          out << "{"
          keys.each_with_index do |k, i|
            out << "," if i.positive?
            out << quote(k) << ":"
            write(out, value.key?(k) ? value[k] : value[k.to_sym], canonical, depth + 1)
          end
          out << "}"
        when Binary then write_binary(out, value, canonical)
        else write(out, from_ruby(value), canonical, depth + 1)
        end
      end

      def write_binary(out, bin, canonical)
        encoded = Base64.strict_encode64(bin.data)
        unless canonical
          tagged = { BINARY_KEY => encoded }
          tagged["filename"] = bin.filename if bin.filename
          tagged["content_type"] = bin.content_type if bin.content_type
          return write(out, tagged, false)
        end
        if bin.filename.nil? && bin.content_type.nil?
          out << %({"$bytes":) << quote(encoded) << "}"
        else
          out << %({"$file":{"bytes":) << quote(encoded)
          out << %(,"name":) << (bin.filename ? quote(bin.filename) : "null")
          out << %(,"type":) << (bin.content_type ? quote(bin.content_type) : "null") << "}}"
        end
      end

      # The member that tags bytes in the JSON text of a Binary.
      BINARY_KEY = "$tungsten_binary"

      ESCAPES = { '"' => '\\"', "\\" => "\\\\", "\b" => "\\b", "\f" => "\\f", "\n" => "\\n", "\r" => "\\r", "\t" => "\\t" }.freeze

      # A string literal as JSON.stringify writes it.
      def quote(text)
        body = utf8(text.to_s).gsub(/["\\\u0000-\u001f]/) do |c|
          ESCAPES[c] || format("\\u%04x", c.ord)
        end
        "\"#{body}\""
      end

      # A number as JavaScript writes it (`String(n)`): 1.0 is "1", 1e-7 is
      # "1e-7", 1e21 is "1e+21".
      def number_text(value)
        return value.to_s if value.is_a?(Integer)
        return "null" if value.nan? || value.infinite?

        js_number(value)
      end

      def js_number(value)
        return "0" if value.zero?

        sign = value.negative? ? "-" : ""
        digits, n = decimal(value.abs)
        k = digits.length
        body =
          if k <= n && n <= 21 then digits + ("0" * (n - k))
          elsif n.positive? && n <= 21 then "#{digits[0, n]}.#{digits[n..]}"
          elsif n > -6 && n <= 0 then "0.#{'0' * -n}#{digits}"
          else
            e = n - 1
            head = k > 1 ? "#{digits[0]}.#{digits[1..]}" : digits
            "#{head}e#{e.negative? ? '-' : '+'}#{e.abs}"
          end
        sign + body
      end

      # The shortest digits of a positive float and the position of the
      # decimal point (value = 0.digits * 10^n).
      def decimal(value)
        text = value.to_s
        if text.include?("e")
          mantissa, exp = text.split("e")
          whole, frac = mantissa.split(".")
          digits = (whole + frac.to_s).sub(/0+\z/, "")
          n = exp.to_i + whole.length
        else
          whole, frac = text.split(".")
          frac = frac.to_s
          if whole == "0"
            zeros = frac[/\A0*/].length
            digits = frac[zeros..].sub(/0+\z/, "")
            n = -zeros
          else
            digits = (whole + frac).sub(/0+\z/, "")
            n = whole.length
          end
        end
        digits = "0" if digits.empty?
        stripped = digits.sub(/\A0+/, "")
        n -= digits.length - stripped.length
        [stripped.empty? ? "0" : stripped, n]
      end

      # Structural equality: 1 equals 1.0, member order does not matter.
      def same?(a, b)
        case a
        when Integer, Float then b.is_a?(Numeric) && a == b
        when Array then b.is_a?(Array) && a.length == b.length && a.each_index.all? { |i| same?(a[i], b[i]) }
        when Hash then b.is_a?(Hash) && a.length == b.length && a.all? { |k, v| b.key?(k) && same?(v, b[k]) }
        when Binary then b.is_a?(Binary) && a.data == b.data
        else a == b && a.instance_of?(b.class)
        end
      end

      # same? where either side may be UNSET.
      def same_opt?(a, b)
        return UNSET.equal?(a) && UNSET.equal?(b) if UNSET.equal?(a) || UNSET.equal?(b)

        same?(a, b)
      end

      # A deep copy of a JSON value.
      def copy(value, depth = 0)
        check_depth(depth)
        case value
        when Hash then value.each_with_object({}) { |(k, v), out| out[k.dup] = copy(v, depth + 1) }
        when Array then value.map { |v| copy(v, depth + 1) }
        when String then value.dup
        else value
        end
      end

      def check_depth(depth)
        raise ArgumentError, "the value is nested too deeply" if depth > MAX_DEPTH
      end

      # The JSON type of a value: null, boolean, integer, number, string,
      # array, object or binary.
      def type_of(value)
        case value
        when nil then "null"
        when true, false then "boolean"
        when Integer then "integer"
        when Float then "number"
        when String then "string"
        when Array then "array"
        when Hash then "object"
        when Binary then "binary"
        else "value"
        end
      end

      # The length of a text in UTF-16 code units, the unit JavaScript counts.
      def utf16_length(text)
        text.each_char.sum { |c| c.ord >= 0x10000 ? 2 : 1 }
      end

      # The first `max` UTF-16 code units of a text, never splitting a
      # character.
      def take_units(text, max)
        used = 0
        out = +""
        text.each_char do |c|
          w = c.ord >= 0x10000 ? 2 : 1
          break if used + w > max

          used += w
          out << c
        end
        out
      end

      # The tagged object form of a Binary in arguments (`{"$tungsten_binary":
      # <base64>, "filename"?, "content_type"?}`), as a Binary; UNSET when it
      # is not one.
      def binary_of(value)
        return value if value.is_a?(Binary)
        return UNSET unless value.is_a?(Hash) && value.key?(BINARY_KEY)

        data = value[BINARY_KEY]
        return UNSET unless data.is_a?(String)
        return UNSET unless (value.keys - [BINARY_KEY, "filename", "content_type"]).empty?

        name = value["filename"]
        type = value["content_type"]
        return UNSET unless (name.nil? || name.is_a?(String)) && (type.nil? || type.is_a?(String))

        Binary.new(data: Base64.strict_decode64(data), filename: name, content_type: type)
      rescue ArgumentError
        UNSET
      end
    end
  end
end
