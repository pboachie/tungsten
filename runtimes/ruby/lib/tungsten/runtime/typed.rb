# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # The behaviour of the value objects of generated SDKs (`Data.define`
    # classes that include it and define `wire_names` and `from_json`):
    # `to_h` writes the members by wire name, without the UNSET ones, with
    # nested models, arrays and maps converted and byte strings as base64.
    module Model
      def to_h
        out = {}
        self.class.wire_names.each do |member, wire|
          value = public_send(member)
          out[wire] = Models.dump(value) unless UNSET.equal?(value)
        end
        out
      end

      def to_json(*) = Json.text(to_h)
    end

    # The helpers generated models decode with. Each keeps a value it
    # cannot decode (nil, UNSET, another shape) as it is.
    module Models
      module_function

      # Every item of an array, decoded by the block.
      def array(value)
        value.is_a?(Array) ? value.map { |item| yield item } : value
      end

      # Every member value of an object, decoded by the block.
      def map(value)
        value.is_a?(Hash) ? value.transform_values { |item| yield item } : value
      end

      # A base64 string as a binary String.
      def bytes(value)
        return value unless value.is_a?(String)

        Base64.strict_decode64(value).force_encoding(Encoding::BINARY)
      rescue ArgumentError
        value
      end

      # The index of the variant of the union named `type_id` the value is
      # (by its tag, else the first variant that accepts it), or nil.
      def variant(descriptors, type_id, value)
        schema = descriptors.schema_of(type_id)
        return nil if UNSET.equal?(schema)

        union = descriptors.defs[type_id]
        return nil unless union.is_a?(Hash) && union["kind"] == "union"

        variants = union["variants"] || []
        if (tag = union["tag"])
          return nil unless value.is_a?(Hash)

          index = (union["tags"] || []).index(value[tag])
          return index && index < variants.length ? index : nil
        end
        variants.index { |v| Schema.new(v, descriptors.defs).accepts?(value) }
      end

      # A typed value back to JSON: models by wire name, arrays and maps
      # converted, byte strings as base64.
      def dump(value)
        case value
        when Model then value.to_h
        when Array then value.reject { |v| UNSET.equal?(v) }.map { |v| dump(v) }
        when Hash then value.reject { |_, v| UNSET.equal?(v) }.transform_values { |v| dump(v) }
        when String then value.encoding == Encoding::BINARY ? Base64.strict_encode64(value) : value
        else value
        end
      end
    end

    # Typed results over the dynamic ones of the core.
    module Typed
      module_function

      # The Ok of a call with its value decoded by the block (`raw` keeps
      # the wire value); an Err as it is.
      def result(result)
        return result unless result.is_a?(Ok)
        return result if result.raw.nil?

        result.with(value: yield(result.raw))
      end

      # The pages of an enumeration with their items decoded by the block.
      def pages(enum)
        Enumerator.new do |y|
          enum.each do |page|
            if page.is_a?(Ok)
              items = page.value.items.map { |item| yield item }
              y << page.with(value: page.value.with(items: items), raw: page.value)
            else
              y << page
            end
          end
        end
      end
    end
  end
end
