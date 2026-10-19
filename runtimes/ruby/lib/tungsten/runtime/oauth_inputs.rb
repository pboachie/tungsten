# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # Bounded parsing and immutable captures used only by managed OAuth.
    module OAuthInputs
      CAPACITY = 32
      INPUT_BYTES = 32 * 1024
      CALLBACK_BYTES = 16 * 1024
      FIELDS = 64
      LIFETIME_MS = 600_000
      SAFE_INTEGER = 9_007_199_254_740_991
      PROTECTED = %w[state code code_verifier code_challenge code_challenge_method client_id client_secret
                     redirect_uri grant_type response_type scope resource audience iss error error_description error_uri].freeze
      class Invalid < StandardError; end

      module_function

      def require_valid(value)
        raise Invalid unless value
      end

      def safe_integer?(value) = value.is_a?(Integer) && value.positive? && value <= SAFE_INTEGER

      # Check original bytes before copying. Managed text is UTF-8, including
      # binary/ASCII strings whose bytes form valid UTF-8; no unbounded transcode.
      def text(value, remaining)
        require_valid(value.is_a?(String) && value.bytesize <= remaining)
        require_valid([Encoding::UTF_8, Encoding::US_ASCII, Encoding::BINARY].include?(value.encoding))
        # Bound the copy even if another application thread grows the string
        # after the size check; normalize String subclasses to an owned String.
        copied = String.new(value.byteslice(0, remaining + 1), encoding: Encoding::UTF_8)
        require_valid(copied.bytesize <= remaining)
        require_valid(copied.valid_encoding?)
        copied.freeze
      end

      class Budget
        def initialize = @remaining = INPUT_BYTES

        def text(value)
          copied = OAuthInputs.text(value, @remaining)
          @remaining -= copied.bytesize
          copied
        end

        def optional(value) = value.nil? ? nil : text(value)

        def scopes(value)
          OAuthInputs.require_valid(value.is_a?(Array) && value.length <= FIELDS)
          Array.new(value.length) { |index| text(value[index]) }.freeze
        end

        def extra(value)
          OAuthInputs.require_valid(value.is_a?(Hash) && value.length <= FIELDS)
          result = {}
          count = 0
          value.each do |key, item|
            count += 1
            OAuthInputs.require_valid(count <= FIELDS)
            name = text(key)
            copied = text(item)
            OAuthInputs.require_valid(!name.empty? && !PROTECTED.include?(name.downcase) && !result.key?(name))
            result[name] = copied
          end
          result.freeze
        end
      end

      def decode(value)
        require_valid(!value.match?(/%(?![0-9a-fA-F]{2})/))
        bytes = value.b.tr("+", " ").gsub(/%([0-9a-fA-F]{2})/) { [$1.to_i(16)].pack("C") }
        decoded = bytes.force_encoding(Encoding::UTF_8)
        require_valid(decoded.valid_encoding?)
        decoded.freeze
      end

      # Count before split; charge every decoded pair before duplicate refusal.
      def query(raw, limit)
        return {}.freeze if raw.nil? || raw.empty?
        require_valid(raw.bytesize <= limit && raw.count("&") < FIELDS)
        result = {}
        remaining = limit
        raw.split("&", -1).each do |field|
          key, value = field.split("=", 2)
          require_valid(!key.empty?)
          name, item = decode(key), decode(value || "")
          remaining -= name.bytesize + item.bytesize
          require_valid(remaining >= 0 && !result.key?(name))
          result[name] = item
        end
        result.freeze
      end

      def uri(value, redirect: false)
        require_valid(!value.empty? && !value.match?(/[\x00-\x20\x7f]/))
        parsed = URI.parse(value)
        require_valid(parsed.absolute? && parsed.userinfo.nil? && parsed.fragment.nil?)
        if %w[http https].include?(parsed.scheme&.downcase)
          require_valid(parsed.host && !parsed.host.empty? && parsed.port&.positive?)
        else
          require_valid(redirect)
        end
        parsed
      rescue URI::Error, ArgumentError
        raise Invalid
      end

      def endpoint(value, redirect: false)
        parsed = uri(value, redirect: redirect)
        query(parsed.query, INPUT_BYTES).each_key { |key| require_valid(!PROTECTED.include?(key.downcase)) }
        value
      end

      def callback(raw, capture)
        value = text(raw, CALLBACK_BYTES)
        parsed = uri(value, redirect: true)
        require_valid(value.split("?", 2).first == capture[:redirect].split("?", 2).first)
        fields = query(parsed.query, CALLBACK_BYTES)
        registered = query(URI.parse(capture[:redirect]).query, CALLBACK_BYTES)
        registered.each { |key, item| require_valid(fields[key] == item) }
        require_valid(fields["state"] == capture[:state])
        require_valid(fields["iss"] == capture[:issuer]) if capture[:issuer_mode] == :response
        if fields.key?("code")
          require_valid(!fields["code"].empty? && %w[error error_description error_uri].none? { |key| fields.key?(key) })
          [:code, fields["code"]]
        else
          require_valid(fields["error"].is_a?(String) && !fields["error"].empty?)
          [:denied, nil]
        end
      end
    end
  end
end
