# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # Predicates and expressions of verification hooks, polls and macros.
    #
    # Predicates (verify `expect` and `terminal`, poll `until`): field path
    # to {"in": [...]} | {"equals": x} | {"contains": ...}; any other value is
    # compared for equality. Every entry must hold.
    #
    # Expressions: JSON in which a string starting with "$" is a reference
    # resolved against a scope ($input.a.b, $<name>.a, $response.x, $args.y),
    # an object {"expr": "<ref> in [..]" | "<ref> == x" | "<ref> != x"} is a
    # boolean, anything else a literal. A dry evaluation (macro previews)
    # knows the input but not the earlier results: a reference to one is the
    # placeholder "<from step NAME: path>".
    module Expr
      module_function

      # Whether every entry of the predicate holds on the value (UNSET when
      # absent). A nil predicate always holds; a malformed one never does.
      def predicate?(predicate, value)
        case predicate
        when nil then true
        when Hash then predicate.all? { |path, test| holds?(test, Util.get_path_str(value, path)) }
        else false
        end
      end

      def subset?(value, pattern)
        case pattern
        when Hash
          value.is_a?(Hash) && pattern.all? { |k, v| value.key?(k) && subset?(value[k], v) }
        when Array
          value.is_a?(Array) && pattern.all? { |want| value.any? { |have| subset?(have, want) } }
        else
          !UNSET.equal?(value) && Json.same?(value, pattern)
        end
      end

      def holds?(test, actual)
        if test.is_a?(Hash) && test.length == 1
          if test.key?("in")
            list = test["in"]
            return list.is_a?(Array) && list.any? { |c| Json.same_opt?(c, actual) }
          end
          return Json.same_opt?(test["equals"], actual) if test.key?("equals")

          if test.key?("contains")
            pattern = test["contains"]
            if pattern.is_a?(String)
              return actual.include?(pattern) if actual.is_a?(String)
              return actual.any? { |i| i == pattern } if actual.is_a?(Array)

              return false
            end
            return actual.any? { |item| subset?(item, pattern) } if actual.is_a?(Array) && !pattern.is_a?(Array)

            return subset?(actual, pattern)
          end
        end
        Json.same_opt?(test, actual)
      end

      # One $name.path reference, or UNSET.
      def resolve_ref(reference, scope)
        segments = Util.split_path(reference.delete_prefix("$"))
        return UNSET if segments.empty? || !scope.key?(segments.first)

        Util.get_path(scope[segments.first], segments.drop(1))
      end

      # An expression's value; unknown references are UNSET (dropped from
      # objects, nil in arrays); a malformed expr string is nil.
      def evaluate(expr, scope, depth = 0)
        return nil if depth > 64

        case expr
        when String then expr.start_with?("$") ? resolve_ref(expr, scope) : expr
        when Array then expr.map { |item| evaluate(item, scope, depth + 1).then { |v| UNSET.equal?(v) ? nil : v } }
        when Hash
          return boolean(expr["expr"], scope) if expr.length == 1 && expr["expr"].is_a?(String)

          expr.each_with_object({}) do |(k, v), out|
            value = evaluate(v, scope, depth + 1)
            out[k] = value unless UNSET.equal?(value)
          end
        else expr
        end
      end

      def placeholder(name, path) = path.empty? ? "<from step #{name}>" : "<from step #{name}: #{path}>"

      # [name, path] of a reference to a pending binding, else nil.
      def pending_ref(reference, pending)
        return nil unless reference.is_a?(String) && reference.start_with?("$")

        segs = Util.split_path(reference[1..])
        return nil if segs.empty?

        name = segs.first
        return nil unless pending.include?(name) && reference.start_with?("$#{name}")

        [name, reference[(name.length + 1)..].delete_prefix(".")]
      end

      # A dry evaluation: references to the pending bindings become
      # placeholders.
      def evaluate_dry(expr, scope, pending, depth = 0)
        return nil if depth > 64

        case expr
        when String
          return expr unless expr.start_with?("$")

          ref = pending_ref(expr, pending)
          ref ? placeholder(*ref) : resolve_ref(expr, scope)
        when Array then expr.map { |item| evaluate_dry(item, scope, pending, depth + 1).then { |v| UNSET.equal?(v) ? nil : v } }
        when Hash
          if expr.length == 1 && expr["expr"].is_a?(String)
            source = expr["expr"].strip
            head = source[/\A\$[^\s=!]+/]
            ref = head && pending_ref(head, pending)
            return placeholder(ref[0], source[(ref[0].length + 1)..].delete_prefix(".")) if ref

            return boolean(source, scope)
          end
          expr.each_with_object({}) do |(k, v), out|
            value = evaluate_dry(v, scope, pending, depth + 1)
            out[k] = value unless UNSET.equal?(value)
          end
        else expr
        end
      end

      # A predicate in words: state in ["a", "b"], endpoints contains {...},
      # a = 1, joined with "and".
      def describe(predicate)
        return "" unless predicate.is_a?(Hash)

        predicate.map do |path, test|
          if test.is_a?(Hash) && test.length == 1 && test["in"].is_a?(Array)
            "#{path} in [#{test['in'].map { |v| Json.text(v) }.join(', ')}]"
          elsif test.is_a?(Hash) && test.length == 1 && test.key?("equals")
            "#{path} = #{Json.text(test['equals'])}"
          elsif test.is_a?(Hash) && test.length == 1 && test.key?("contains")
            "#{path} contains #{Json.text(test['contains'])}"
          else
            "#{path} = #{Json.text(test)}"
          end
        end.join(" and ")
      end

      def literal(text)
        trimmed = text.strip
        value = Json.try_parse(trimmed)
        return value unless UNSET.equal?(value)

        normalized = trimmed.gsub(/'((?:[^'\\]|\\.)*)'/) { Json.quote(Regexp.last_match(1).gsub("\\'", "'")) }
        Json.try_parse(normalized)
      end

      BOOLEAN = /\A\s*(\$[^\s=!]+)\s+(in|==|!=)\s+(.+)\z/m

      # `<ref> in [..]`, `<ref> == x` or `<ref> != x`; nil when malformed.
      def boolean(source, scope)
        m = source.match(BOOLEAN)
        return nil unless m

        lit = literal(m[3])
        return nil if UNSET.equal?(lit)

        actual = resolve_ref(m[1], scope)
        if m[2] == "in"
          return nil unless lit.is_a?(Array)

          return lit.any? { |v| Json.same_opt?(v, actual) }
        end
        actual = nil if UNSET.equal?(actual)
        same = Json.same?(actual, lit)
        m[2] == "==" ? same : !same
      end
    end
  end
end
