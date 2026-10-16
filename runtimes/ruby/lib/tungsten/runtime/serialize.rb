# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # Parameter and body serialization, and the request's headers.
    module Serialize
      # A value that cannot be encoded becomes VALIDATION_FAILED.
      SerializationError = Data.define(:parameter, :expected, :value)

      # A body ready for the wire: `bytes` (a binary String) or `parts`
      # (multipart), `content_type`, the redacted `display` and the bytes the
      # content hash is computed over (`hashed`, nil without a body).
      Encoded = Data.define(:empty, :bytes, :parts, :content_type, :display, :hashed)
      EMPTY = Encoded.new(empty: true, bytes: nil, parts: nil, content_type: nil, display: nil, hashed: nil)

      # One multipart part: `kind` :text, :json or :file.
      Part = Data.define(:name, :kind, :text, :data, :filename, :content_type)

      module_function

      # String(value) for a scalar, JSON text for anything else.
      def scalar(value)
        case value
        when String then value
        when Integer, Float then Json.number_text(value)
        when true, false then value.to_s
        else Json.text(value)
        end
      end

      def escape(text, keep)
        text.to_s.b.each_byte.map do |b|
          c = b.chr
          c.match?(/[A-Za-z0-9]/) || keep.include?(c) ? c : format("%%%02X", b)
        end.join
      end

      # encodeURIComponent.
      def encode_component(text) = escape(text, "-_.!~*'()")

      # One component of a form body (URLSearchParams): spaces as `+`.
      def encode_form_component(text)
        text.to_s.b.each_byte.map do |b|
          c = b.chr
          if c.match?(/[A-Za-z0-9]/) || "*-._".include?(c) then c
          elsif c == " " then "+"
          else format("%%%02X", b)
          end
        end.join
      end

      # RFC 3986 percent-encoding.
      def percent_encode(text) = escape(text, "-._~")

      def encoded_items(items) = items.reject(&:nil?).map { |v| encode_component(scalar(v)) }

      def encoded_pairs(object)
        object.reject { |_, v| v.nil? }.map { |k, v| [encode_component(k), encode_component(scalar(v))] }
      end

      # One path parameter value, percent-encoded.
      def path_param(p, value)
        name = encode_component(p.wire)
        style = %w[label matrix].include?(p.style) ? p.style : "simple"
        case value
        when Array
          items = encoded_items(value)
          return ".#{items.join(p.explode ? '.' : ',')}" if style == "label"
          return items.map { |i| ";#{name}=#{i}" }.join if style == "matrix" && p.explode
          return ";#{name}=#{items.join(',')}" if style == "matrix"

          items.join(",")
        when Hash
          pairs = encoded_pairs(value)
          exploded = pairs.map { |k, v| "#{k}=#{v}" }
          flat = pairs.flatten
          return ".#{exploded.join('.')}" if style == "label" && p.explode
          return ".#{flat.join(',')}" if style == "label"
          return exploded.map { |e| ";#{e}" }.join if style == "matrix" && p.explode
          return ";#{name}=#{flat.join(',')}" if style == "matrix"

          p.explode ? exploded.join(",") : flat.join(",")
        else
          val = encode_component(scalar(value))
          case style
          when "label" then ".#{val}"
          when "matrix" then ";#{name}=#{val}"
          else val
          end
        end
      end

      def deep_object_pairs(prefix, value, out, depth)
        return if depth > 16 || value.nil?

        case value
        when Array then value.each { |item| deep_object_pairs(prefix, item, out, depth + 1) }
        when Hash
          value.each do |k, v|
            deep_object_pairs("#{prefix}[#{encode_component(k)}]", v, out, depth + 1) unless v.nil?
          end
        else out << "#{prefix}=#{encode_component(scalar(value))}"
        end
      end

      # The query parts (name=value, encoded) of one parameter.
      def query_param(p, value)
        return [] if value.nil?

        name = encode_component(p.wire)
        delimiter = { "space_delimited" => "%20", "pipe_delimited" => "|" }.fetch(p.style, ",")
        if value.is_a?(Hash) && p.style == "deep_object"
          out = []
          deep_object_pairs(name, value, out, 0)
          return out
        end
        case value
        when Array
          items = encoded_items(value)
          return [] if items.empty?
          return items.map { |i| "#{name}=#{i}" } if p.explode || p.style == "deep_object"

          ["#{name}=#{items.join(delimiter)}"]
        when Hash
          pairs = encoded_pairs(value)
          return pairs.map { |k, v| "#{k}=#{v}" } if p.explode

          ["#{name}=#{pairs.flatten.join(delimiter)}"]
        else ["#{name}=#{encode_component(scalar(value))}"]
        end
      end

      # A header parameter (simple style, not encoded).
      def header_param(p, value)
        case value
        when Array then value.reject(&:nil?).map { |v| scalar(v) }.join(",")
        when Hash
          value.reject { |_, v| v.nil? }.flat_map { |k, v| p.explode ? ["#{k}=#{scalar(v)}"] : [k, scalar(v)] }.join(",")
        else scalar(value)
        end
      end

      # name=value of a cookie parameter.
      def cookie_param(p, value)
        case value
        when Array then "#{p.wire}=#{encoded_items(value).join(',')}"
        when Hash then "#{p.wire}=#{encoded_pairs(value).flatten.join(',')}"
        else "#{p.wire}=#{encode_component(scalar(value))}"
        end
      end

      # The body before encoding: merged fields picked from the arguments
      # (under their wire names) or the body argument; wrapped in the rpc
      # envelope when the operation has one. UNSET without a body.
      def body_value(op, args, param_names)
        value = UNSET
        if (b = op.body)
          if b.arg
            value = args.fetch(b.arg, UNSET)
          else
            picked = {}
            b.fields.each { |name, wire| picked[wire] = args[name] if args.key?(name) }
            value = picked if !picked.empty? || b.required || op.rpc
          end
        end
        rpc = op.rpc
        return value unless rpc

        params =
          if op.body.nil? then args.reject { |k, _| param_names.include?(k) }
          elsif !UNSET.equal?(value) then value
          else {}
          end
        envelope = {}
        (rpc["constants"] || {}).each { |k, v| envelope[k] = v }
        envelope[rpc["field"]] = rpc["value"]
        envelope[rpc["params_field"]] = params
        envelope
      end

      def binary_display(n, media) = media.to_s.empty? ? "<#{n} bytes>" : "<#{n} bytes of #{media}>"

      def form_pairs(value)
        return SerializationError.new(parameter: "body", expected: "an object of form fields", value: value) unless value.is_a?(Hash)

        out = []
        value.each do |k, item|
          next if item.nil?

          if item.is_a?(Array)
            item.each { |x| out << [k, scalar(x)] unless x.nil? }
          else
            out << [k, scalar(item)]
          end
        end
        out
      end

      # Encode a body per its descriptor; a SerializationError when the value
      # does not fit. `redact` renders the shown body.
      def encode_body(desc, value, redact)
        return EMPTY if UNSET.equal?(value)

        encoding = desc ? desc.encoding : "json"
        media = desc ? desc.media_type : "application/json"
        case encoding
        when "form"
          pairs = form_pairs(value)
          return pairs if pairs.is_a?(SerializationError)

          text = pairs.map { |k, v| "#{encode_form_component(k)}=#{encode_form_component(v)}" }.join("&")
          Encoded.new(empty: false, bytes: text.b, parts: nil, content_type: media, display: redact.call(pairs.to_h),
                      hashed: text.b)
        when "multipart" then multipart(value, redact)
        when "bytes"
          bin = Json.binary_of(value)
          data =
            if value.is_a?(String) then value.b
            elsif !UNSET.equal?(bin) then bin.data
            else
              return SerializationError.new(parameter: "body", expected: "binary data (a Binary value)", value: value)
            end
          Encoded.new(empty: false, bytes: data, parts: nil, content_type: media,
                      display: binary_display(data.bytesize, media), hashed: data)
        when "text"
          unless value.is_a?(String) || value.is_a?(Numeric) || [true, false].include?(value)
            return SerializationError.new(parameter: "body", expected: "a string", value: value)
          end

          text = scalar(value)
          Encoded.new(empty: false, bytes: text.b, parts: nil, content_type: media, display: text, hashed: text.b)
        else
          text = Json.text(value)
          raw = Json.try_parse(text)
          if UNSET.equal?(raw)
            return SerializationError.new(parameter: "body", expected: "a JSON-serializable value", value: value)
          end

          Encoded.new(empty: false, bytes: text.b, parts: nil, content_type: media, display: redact.call(raw),
                      hashed: Json.canonical(raw).b)
        end
      end

      def multipart(value, redact)
        unless value.is_a?(Hash)
          return SerializationError.new(parameter: "body", expected: "an object of multipart fields", value: value)
        end

        parts = []
        display = {}
        value.each do |k, item|
          next if item.nil?

          list = item.is_a?(Array) ? item.reject(&:nil?) : [item]
          shown = []
          list.each do |x|
            bin = Json.binary_of(x)
            if !UNSET.equal?(bin)
              content_type = bin.content_type || "application/octet-stream"
              unless valid_media_type?(content_type)
                return SerializationError.new(parameter: "body", expected: "multipart file content types that are valid media types",
                                              value: value)
              end

              # Keep the checked header value stable until the body is sent.
              content_type = content_type.dup.freeze
              shown << binary_display(bin.bytesize, bin.content_type)
              parts << Part.new(name: k, kind: :file, text: nil, data: bin.data, filename: bin.filename || k,
                                content_type: content_type)
            elsif x.is_a?(Hash)
              parts << Part.new(name: k, kind: :json, text: Json.text(x), data: nil, filename: nil, content_type: nil)
              shown << x
            else
              parts << Part.new(name: k, kind: :text, text: scalar(x), data: nil, filename: nil, content_type: nil)
              shown << scalar(x)
            end
          end
          display[k] = item.is_a?(Array) ? shown : shown.first
        end
        Encoded.new(empty: false, bytes: nil, parts: parts, content_type: nil, display: redact.call(display),
                    hashed: Json.canonical(value).b)
      end

      # The bytes of a multipart body and its content type.
      def multipart_body(parts)
        boundary = SecureRandom.hex(16)
        out = +"".b
        quote = ->(s) { s.to_s.gsub("\\", "\\\\\\\\").gsub('"', '\\"').gsub("\r", "%0D").gsub("\n", "%0A") }
        parts.each do |p|
          out << "--#{boundary}\r\n".b
          case p.kind
          when :file
            out << %(Content-Disposition: form-data; name="#{quote.call(p.name)}"; filename="#{quote.call(p.filename)}"\r\n).b
            out << "Content-Type: #{p.content_type}\r\n\r\n".b << p.data.b
          when :json
            out << %(Content-Disposition: form-data; name="#{quote.call(p.name)}"\r\n).b
            out << "Content-Type: application/json\r\n\r\n".b << p.text.b
          else
            out << %(Content-Disposition: form-data; name="#{quote.call(p.name)}"\r\n\r\n).b << p.text.b
          end
          out << "\r\n".b
        end
        out << "--#{boundary}--\r\n".b
        [out, "multipart/form-data; boundary=#{boundary}"]
      end

      # An RFC 9110 token.
      def valid_header_name?(name) = name.is_a?(String) && name.match?(/\A[A-Za-z0-9!#$%&'*+\-.^_`|~]+\z/)

      # Visible ASCII, tab, space and Latin-1 characters.
      def valid_header_value?(value)
        value.is_a?(String) && value.each_char.all? { |c| c == "\t" || (c.ord >= 0x20 && c.ord <= 0x7e) || (c.ord >= 0x80 && c.ord <= 0xff) }
      end

      # `type/subtype` (RFC 9110 tokens) optionally followed by `;` parameters,
      # all in visible ASCII, space and tab.
      def valid_media_type?(value)
        return false unless value.is_a?(String) && value.match?(/\A[\t\x20-\x7e]*\z/)

        value.split(";", 2).first.strip.match?(%r{\A[!#$%&'*+\-.^_`|~0-9A-Za-z]+/[!#$%&'*+\-.^_`|~0-9A-Za-z]+\z})
      end

      # The Latin-1 bytes of a valid header value.
      def header_bytes(value) = value.each_char.map { |c| c.ord.chr }.join.b
    end

    # A case-insensitive header collection that remembers which values are
    # secrets, in insertion order.
    class HeaderBag
      Entry = Struct.new(:name, :value, :secret)

      attr_reader :entries

      def initialize(entries = [])
        @entries = entries.map(&:dup)
      end

      def set(name, value, secret)
        existing = @entries.find { |e| e.name.casecmp?(name) }
        if existing
          existing.name = name
          existing.value = value
          existing.secret = secret
        else
          @entries << Entry.new(name, value, secret)
        end
      end

      def get(name) = @entries.find { |e| e.name.casecmp?(name) }&.value

      def delete(name) = @entries.reject! { |e| e.name.casecmp?(name) }

      def to_h(redact) = @entries.to_h { |e| [e.name, redact && e.secret ? REDACTED : e.value] }

      def secrets = @entries.select(&:secret).map(&:value)

      def dup = HeaderBag.new(@entries)
    end
  end
end
