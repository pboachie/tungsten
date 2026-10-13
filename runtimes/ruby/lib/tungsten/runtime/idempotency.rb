# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # Keeps the idempotency key of each logical call for the life of the
    # process, so a retry of the same intent reuses it. Any object with
    # `get(scope, logical_id)` and `put(scope, logical_id, key)` can replace
    # it (ClientOptions `idempotency_store`).
    class MemoryIdempotencyStore
      def initialize
        @keys = {}
        @lock = Mutex.new
      end

      # The key stored for a logical call, or nil.
      def get(scope, logical_id) = @lock.synchronize { @keys[[scope, logical_id]] }

      def put(scope, logical_id, key)
        @lock.synchronize { @keys[[scope, logical_id]] = key }
      end
    end

    # Idempotency key formats and confirmation tokens.
    module Keys
      module_function

      def format_kind(format)
        case format.to_s.downcase.gsub(/[^a-z0-9]/, "")
        when "uuidv4", "uuid4" then "uuid_v4"
        when "uuid" then "uuid"
        when "asciitoken128" then "ascii_token_128"
        else "token"
        end
      end

      # What a policy's key format expects, in words.
      def format_description(format)
        case format_kind(format)
        when "uuid_v4" then "UUIDv4 string"
        when "uuid" then "UUID string"
        when "ascii_token_128" then "1-128 ASCII letters, digits, dots, underscores or hyphens"
        else "a non-empty printable ASCII string of at most 255 characters"
        end
      end

      def uuid?(key, v4_only)
        version = v4_only ? "4" : "[1-8]"
        key.is_a?(String) && key.match?(/\A\h{8}-\h{4}-#{version}\h{3}-[89abAB]\h{3}-\h{12}\z/)
      end

      # The expected format when the key does not match it, else nil.
      def check_format(key, format)
        valid =
          case format_kind(format)
          when "uuid_v4" then uuid?(key, true)
          when "uuid" then uuid?(key, false)
          when "ascii_token_128" then key.is_a?(String) && key.match?(/\A[A-Za-z0-9._-]{1,128}\z/)
          else key.is_a?(String) && !key.empty? && key.bytesize <= 255 && key.match?(/\A[\x21-\x7e]+\z/)
          end
        valid ? nil : format_description(format)
      end

      # The wire header of an operation's key.
      def header(op)
        h = op.agent["idempotency"]["header"]
        return h if h.is_a?(String) && !h.empty?

        op.params.find { |p| p.role == "idempotency_key" }&.wire || "Idempotency-Key"
      end

      # The lifetime of a confirmation token.
      TTL_MS = 5 * 60 * 1000
      PREFIX = "tgc1"

      # The SHA-256 of the canonical JSON of the arguments.
      def args_digest(args) = Util.sha256_hex(Json.canonical(args))

      def signature(key, operation, digest, expiry)
        Util.base64url(Util.hmac_sha256(key, "#{operation}\n#{digest}\n#{expiry}"))
      end

      # tgc1.<expiry ms>.<base64url(HMAC-SHA-256(key, operation, args digest,
      # expiry))>: valid only for the operation and the exact arguments.
      def issue_token(key, operation, args, now)
        expiry = now + TTL_MS
        "#{PREFIX}.#{expiry}.#{signature(key, operation, args_digest(args), expiry)}"
      end

      # [expiry, signature] of a token of the right shape, else nil.
      def token_expiry(token)
        m = token.to_s.match(/\A#{PREFIX}\.(\d{1,16})\.([A-Za-z0-9_-]{43})\z/o)
        return nil unless m && m[1].to_i.to_s == m[1]

        [m[1].to_i, m[2]]
      end

      # :valid, :expired, :mismatch or :malformed.
      def check_token(key, token, operation, args, now)
        parsed = token_expiry(token)
        return :malformed unless parsed

        expiry, given = parsed
        return :mismatch unless Util.timing_safe_equal?(signature(key, operation, args_digest(args), expiry), given)

        expiry > now ? :valid : :expired
      end
    end
  end
end
