# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # One validation problem: the path of the offending value (member names
    # and array indexes) and a message.
    Issue = Data.define(:path, :message)

    # The validator of the runtime schema form of the SDK descriptor
    # document (`tungsten_emit::sdk::descriptors` specifies it): arguments
    # before any network call, success bodies, page items and events after.
    #
    # Issues come structure first, then constraint violations, then unknown
    # members. Types are strict (no coercion); string lengths count UTF-16
    # code units. `pattern` is an ECMAScript expression run on Onigmo, Ruby's
    # engine: `^` and `$` outside a character class are anchored to the
    # whole string (`\A`, `\z`) as in ECMAScript without the `m` flag; other
    # differences (Unicode property and case-folding details, `\d`, `\w` and
    # `\s` being ASCII-only in Ruby) are Onigmo's, and a pattern Onigmo
    # cannot compile is not checked.
    class Schema
      DATE = "(?:(?:[0-9][0-9][2468][048]|[0-9][0-9][13579][26]|[0-9][0-9]0[48]|[02468][048]00" \
             "|[13579][26]00)-02-29|[0-9]{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12][0-9]|3[01])" \
             "|(?:0[469]|11)-(?:0[1-9]|[12][0-9]|30)|(?:02)-(?:0[1-9]|1[0-9]|2[0-8])))"
      IPV4_PART = "(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])"
      IPV6 = ("(?:(?:H:){7}H|(?:H:){1,7}:|(?:H:){1,6}:H|(?:H:){1,5}(?::H){1,2}" \
              "|(?:H:){1,4}(?::H){1,3}|(?:H:){1,3}(?::H){1,4}|(?:H:){1,2}(?::H){1,5}" \
              "|H:(?:(?::H){1,6})|:(?:(?::H){1,7}|:))").gsub("H", "[0-9a-fA-F]{1,4}")
      FORMATS = {
        "uuid" => "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
        "email" => "[^\\s@\"]{1,64}@[^\\s@]{1,255}",
        "date-time" => "#{DATE}T(?:[01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9](?:\\.[0-9]+)?(?:Z|[+-](?:[01][0-9]|2[0-3]):[0-5][0-9])",
        "date" => DATE,
        "ipv4" => "(?:#{IPV4_PART}\\.){3}#{IPV4_PART}",
        "ipv6" => IPV6
      }.transform_values { |src| Regexp.new("\\A(?:#{src})\\z") }.freeze

      @patterns = {}
      @patterns_lock = Mutex.new

      class << self
        # The compiled pattern of an ECMAScript source, or nil when Onigmo
        # cannot compile it.
        def pattern(source)
          @patterns_lock.synchronize do
            return @patterns[source] if @patterns.key?(source)

            @patterns[source] = begin
              Regexp.new(translate(source))
            rescue RegexpError, ArgumentError
              nil
            end
          end
        end

        # `^` and `$` outside a character class as `\A` and `\z`.
        def translate(source)
          out = +""
          in_class = false
          escaped = false
          source.each_char do |c|
            if escaped
              out << c
              escaped = false
            elsif c == "\\"
              out << c
              escaped = true
            elsif in_class
              in_class = false if c == "]"
              out << c
            elsif c == "["
              in_class = true
              out << c
            elsif c == "^"
              out << "\\A"
            elsif c == "$"
              out << "\\z"
            else
              out << c
            end
          end
          out
        end
      end

      # A validator of `schema` (a Hash of the runtime schema form) whose
      # `ref`s resolve in `defs`.
      def initialize(schema, defs)
        @schema = schema
        @defs = defs || {}
      end

      # The issues of a value, none when it is valid.
      def validate(value)
        run(@schema, value)
      end

      # Whether the value is valid.
      def accepts?(value) = validate(value).empty?

      def run(schema, value)
        # Descriptors share validators across clients. Keep each validation's
        # issue lists on its own worker, including recursive union probes.
        self.class.new(schema, @defs).send(:run_validation, schema, value)
      end

      # Whether `schema` admits `value`, in a fresh run (union variants).
      def admits?(schema, value)
        run(schema, value).empty?
      end

      private

      def run_validation(schema, value)
        @structure = []
        @constraints = []
        @unknown = []
        check(schema, value, [], 0)
        @structure + @constraints + @unknown
      end

      def bad(path, message) = @structure << Issue.new(path: path, message: message)
      def limit(path, message) = @constraints << Issue.new(path: path, message: message)

      def describe(value)
        case value
        when nil then "null"
        when true, false then "a boolean"
        when Numeric then "a number"
        when String then "a string"
        when Array then "an array"
        when Hash then "an object"
        when Binary then "binary data"
        else "a value"
        end
      end

      def check(schema, value, path, depth)
        return if schema.nil?
        return bad(path, "the value is nested too deeply") if depth > 512

        case schema["kind"]
        when nil, "any" then nil
        when "never" then bad(path, "no value is allowed here")
        when "nullable" then check(schema["inner"], value, path, depth + 1) unless value.nil?
        when "ref"
          target = @defs[schema["ref"]]
          check(target, value, path, depth + 1) if target
        when "string"
          return bad(path, "expected a string, found #{describe(value)}") unless value.is_a?(String)

          string_limits(schema, value, path)
        when "bytes"
          bad(path, "expected binary data, found #{describe(value)}") unless value.is_a?(Binary) || value.is_a?(String) || !UNSET.equal?(Json.binary_of(value))
        when "boolean"
          bad(path, "expected a boolean, found #{describe(value)}") unless [true, false].include?(value)
        when "integer" then integer(schema, value, path)
        when "number"
          return bad(path, "expected a number, found #{describe(value)}") unless value.is_a?(Integer) || value.is_a?(Float)

          number_limits(schema, value, path)
        when "enum"
          values = schema["values"] || []
          bad(path, "must be one of #{Json.text(values)}") unless values.any? { |v| Json.same?(v, value) }
        when "const"
          values = schema["values"] || []
          bad(path, "must be #{Json.text(values[0])}") if !values.empty? && !Json.same?(values[0], value)
        when "array" then array(schema, value, path, depth)
        when "map"
          return bad(path, "expected an object, found #{describe(value)}") unless value.is_a?(Hash)

          value.each { |k, v| check(schema["items"], v, path + [k], depth + 1) }
        when "object"
          return bad(path, "expected an object, found #{describe(value)}") unless value.is_a?(Hash)

          object(schema, value, path, depth)
        when "union" then union(schema, value, path, depth)
        when "all" then (schema["variants"] || []).each { |m| check(m, value, path, depth + 1) }
        when "limits"
          case value
          when String then string_limits(schema, value, path)
          when Integer, Float then number_limits(schema, value, path)
          end
        else bad(path, "unknown schema kind #{schema['kind']}")
        end
      end

      def integer(schema, value, path)
        unless value.is_a?(Integer)
          return bad(path, "expected an integer, found #{Json.js_number(value)}") if value.is_a?(Float)

          return bad(path, "expected an integer, found #{describe(value)}")
        end
        return bad(path, "expected a 32-bit integer") if schema["bits"] == 32 && !value.between?(-(2**31), (2**31) - 1)

        number_limits(schema, value, path)
      end

      def array(schema, value, path, depth)
        return bad(path, "expected an array, found #{describe(value)}") unless value.is_a?(Array)

        n = value.length
        limit(path, "must have at least #{schema['min_items']} items") if schema["min_items"] && n < schema["min_items"]
        if schema["max_items"] && n > schema["max_items"]
          # Reject an oversized array before traversing or comparing its items.
          return limit(path, "must have at most #{schema['max_items']} items")
        end

        value.each_with_index { |item, i| check(schema["items"], item, path + [i], depth + 1) }
        return unless schema["unique"]

        seen = {}
        repeated = value.any? do |item|
          bucket = (seen[uniqueness_hash(item)] ||= [])
          duplicate = bucket.any? { |other| Json.same?(item, other) }
          bucket << item unless duplicate
          duplicate
        end
        limit(path, "items must be unique") if repeated
      end

      # Hash structurally equal JSON values alike, then confirm matches with
      # same? so hash collisions never turn distinct values into duplicates.
      # Numeric forms and Binary metadata differ from Ruby's Hash equality.
      def uniqueness_hash(value)
        case value
        when Integer, Float then [:number, value.to_f].hash
        when Binary then [:binary, value.data].hash
        when Array then [:array, value.map { |item| uniqueness_hash(item) }].hash
        when Hash then [:object, value.transform_values { |item| uniqueness_hash(item) }].hash
        else [value.class, value].hash
        end
      end

      def object(schema, value, path, depth)
        fields = schema["fields"] || []
        known = {}
        fields.each do |f|
          name = f["name"]
          known[name] = true
          at = path + [name]
          if !value.key?(name)
            bad(at, "missing required member #{name}") if f["required"]
          elsif value[name].nil? && f["nullable"]
            nil
          else
            check(f["schema"], value[name], at, depth + 1)
          end
        end
        value.each do |k, v|
          next if known[k]

          if schema["extra"]
            check(schema["extra"], v, path + [k], depth + 1)
          elsif schema["additional"] == "closed"
            @unknown << Issue.new(path: path + [k], message: "unknown member #{k}")
          end
        end
      end

      def union(schema, value, path, depth)
        variants = schema["variants"] || []
        if (tag = schema["tag"])
          return bad(path, "expected an object, found #{describe(value)}") unless value.is_a?(Hash)

          text = value[tag]
          return bad(path + [tag], "missing the discriminator #{tag}") unless text.is_a?(String)

          tags = schema["tags"] || []
          index = tags.index(text)
          return check(variants[index], value, path, depth + 1) if index && index < variants.length

          return bad(path + [tag], "must be one of #{Json.text(tags)}")
        end
        return if variants.any? { |v| admits?(v, value) }

        bad(path, "does not match any variant")
      end

      def bound_text(value)
        value.is_a?(Float) && value == value.truncate && value.abs < Json::MAX_SAFE ? value.to_i.to_s : Json.number_text(value)
      end

      def string_limits(schema, text, path)
        n = Json.utf16_length(text)
        limit(path, "must have at least #{schema['min_length']} characters") if schema["min_length"] && n < schema["min_length"]
        limit(path, "must have at most #{schema['max_length']} characters") if schema["max_length"] && n > schema["max_length"]
        if (source = schema["pattern"]) && (re = Schema.pattern(source)) && !re.match?(text)
          limit(path, "must match the pattern /#{source}/")
        end
        format = FORMATS[schema["format"]]
        limit(path, "must be a valid #{schema['format']}") if format && !format.match?(text)
      end

      def number_limits(schema, number, path)
        limit(path, "must be at least #{bound_text(schema['minimum'])}") if schema["minimum"] && number < schema["minimum"]
        limit(path, "must be at most #{bound_text(schema['maximum'])}") if schema["maximum"] && number > schema["maximum"]
        if schema["exclusive_minimum"] && number <= schema["exclusive_minimum"]
          limit(path, "must be greater than #{bound_text(schema['exclusive_minimum'])}")
        end
        if schema["exclusive_maximum"] && number >= schema["exclusive_maximum"]
          limit(path, "must be less than #{bound_text(schema['exclusive_maximum'])}")
        end
        step = schema["multiple_of"]
        return unless step.is_a?(Numeric) && step != 0

        q = number.to_f / step
        return unless (q - q.round).abs > 1e-9 * [q.abs, 1].max

        limit(path, "must be a multiple of #{bound_text(step)}")
      end
    end

    # The path of an issue as .key and [index] segments.
    def self.issue_path(path)
      path.map { |seg| seg.is_a?(Integer) || Util.index?(seg) ? "[#{seg}]" : ".#{seg}" }.join
    end
  end
end
