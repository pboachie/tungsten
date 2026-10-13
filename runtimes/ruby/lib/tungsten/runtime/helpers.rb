# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # Argument paths, sensitivity and the texts built from arguments.
    module Helpers
      module_function

      # A structural problem that makes a descriptor unusable, or nil.
      def descriptor_problem(op)
        return "`id` must be a non-empty string" if op.id.empty?
        return "every parameter needs a `name` and a `wire`" if op.params.any? { |p| p.name.empty? || p.wire.empty? }
        return "`rpc` needs `field` and `params_field`" if op.rpc && (op.rpc["field"].to_s.empty? || op.rpc["params_field"].to_s.empty?)

        nil
      end

      # The path of an argument issue: body.<wire> for a merged body field,
      # body for the whole-body argument, else args.<name>.
      def argument_path(op, path)
        if op.body && !path.empty?
          head = path.first.to_s
          rest = path.drop(1)
          if op.body.arg
            return "body#{Runtime.issue_path(rest)}" if op.body.arg == head
          else
            field = op.body.fields.find { |name, _| name == head }
            return "body.#{field[1]}#{Runtime.issue_path(rest)}" if field
          end
        end
        return "args" if path.empty?

        "args#{Runtime.issue_path(path)}"
      end

      def sensitive_request_paths(op) = op.agent["sensitive_request_fields"].reject(&:empty?)

      # An argument path without array indexes.
      def dotted(path) = path.reject { |s| s.is_a?(Integer) || s.to_s.match?(/\A\d*\z/) }.map(&:to_s).join(".")

      def sensitive_arg?(op, path)
        d = dotted(path)
        return true if !d.empty? && sensitive_request_paths(op).any? { |f| d == f || d.start_with?("#{f}.") }

        path.any? { |s| s.is_a?(String) && Util.looks_sensitive?(s) }
      end

      # The sensitive fields below the argument at path, redacted.
      def redact_below(op, path, value)
        d = dotted(path)
        prefix = d.empty? ? "" : "#{d}."
        below = sensitive_request_paths(op).filter_map { |f| f.delete_prefix(prefix) if f.start_with?(prefix) }
        below.empty? ? value : Util.redact_paths(value, below)
      end

      # The sensitive argument paths relative to the body as sent; "" is the
      # whole body.
      def sensitive_body_paths(op)
        paths = sensitive_request_paths(op)
        relative = []
        if op.body && op.body.arg.nil?
          paths.each do |p|
            segs = Util.split_path(p)
            next if segs.empty?

            field = op.body.fields.find { |name, _| name == segs.first }
            next unless field

            rest = segs.drop(1).join(".")
            relative << (rest.empty? ? field[1] : "#{field[1]}.#{rest}")
          end
        elsif op.body
          arg = op.body.arg
          paths.each do |p|
            if p == arg then relative << ""
            elsif p.start_with?("#{arg}.") then relative << p.delete_prefix("#{arg}.")
            end
          end
        elsif op.rpc
          relative.concat(paths)
        end
        return relative unless op.rpc

        field = op.rpc["params_field"]
        relative.map { |p| p.empty? ? field : "#{field}.#{p}" }
      end

      # Every string or number found at a path, through arrays.
      def values_at(value, segments, out, depth = 0)
        return if depth > 64

        if value.is_a?(Array)
          value.each { |item| values_at(item, segments, out, depth + 1) }
          return
        end
        if segments.empty?
          case value
          when String then out << value
          when Integer, Float then out << Json.number_text(value)
          when Hash then value.each_value { |v| values_at(v, [], out, depth + 1) }
          end
          return
        end
        values_at(value[segments.first], segments.drop(1), out, depth + 1) if value.is_a?(Hash) && value.key?(segments.first)
      end

      # The arguments with each parameter's value also under its wire name,
      # so references written against the API find arguments whose SDK name
      # differs.
      def with_wire_names(op, args)
        out = args.dup
        op.params.each { |p| out[p.wire] = args[p.name] if args.key?(p.name) && !out.key?(p.wire) }
        out
      end

      # A path with a leading parameter wire name rewritten to its name.
      def arg_path(op, path)
        return path if path.empty?

        p = op.params.find { |x| x.wire == path.first && x.name != path.first }
        p ? [p.name] + path.drop(1) : path
      end

      # {field} references filled from the arguments; sensitive ones are
      # redacted, unset ones shown as <unset>.
      def interpolate(template, args, op)
        template.to_s.gsub(/\{([^{}]+)\}/) do
          segments = arg_path(op, Regexp.last_match(1).strip.split("."))
          value = Util.get_path(args, segments)
          if UNSET.equal?(value) || value.nil?
            "<unset>"
          elsif sensitive_arg?(op, segments)
            REDACTED
          else
            shown = Util.envelope_value(value, false)
            shown.is_a?(String) ? shown : Json.text(shown)
          end
        end
      end

      PLACEHOLDER = /\A<from step [^<>]+>\z/

      def placeholders_in(value, out, depth = 0)
        return if depth > 64

        case value
        when String then out << value if value.match?(PLACEHOLDER)
        when Array then value.each { |v| placeholders_in(v, out, depth + 1) }
        when Hash then value.each_value { |v| placeholders_in(v, out, depth + 1) }
        end
      end

      def placeholder?(value)
        out = []
        placeholders_in(value, out)
        !out.empty?
      end

      # The placeholders of args in a preview URL as written instead of
      # percent-encoded.
      def unescape_placeholders(url, args)
        found = []
        placeholders_in(args, found)
        found.uniq.each { |text| url = url.gsub(Serialize.encode_component(text), text) }
        url
      end

      # " (name value, ...)": the scalar, non-sensitive arguments a call was
      # made with (at most four, by wire name).
      def sent_arguments(op, args)
        shown = []
        args.each do |k, v|
          break if shown.length == 4
          next unless v.is_a?(String) || v.is_a?(Numeric) || [true, false].include?(v)
          next if sensitive_arg?(op, [k])

          wire = op.params.find { |p| p.name == k }&.wire || k
          shown << "#{wire} #{Util.short_json(v)}"
        end
        shown.empty? ? "" : " (#{shown.join(', ')})"
      end
    end
  end
end
