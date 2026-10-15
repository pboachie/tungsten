# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # Raised when a client cannot be built (a bad base URL, an option of the
    # wrong type). Calls never raise; they return Err.
    class ConfigError < StandardError; end

    # Defaults of ClientOptions.
    DEFAULT_TIMEOUT_MS = 30_000
    DEFAULT_MAX_EVENT_BYTES = 1024 * 1024
    DEFAULT_MAX_COLLECT_BYTES = 4 * 1024 * 1024
    DEFAULT_MAX_COLLECT_MS = 60_000
    DEFAULT_MAX_RECONNECTS = 3
    DEFAULT_RECONNECT_MAX_MS = 30_000

    # Retry settings, field by field (nil fields are unset): `max` attempts
    # after the first, `base_ms` and `max_ms` of the backoff, `jitter`
    # (`"none"`, `"full"`, `"equal"`), `honor_retry_after`.
    RetryOptions = Data.define(:max, :base_ms, :max_ms, :jitter, :honor_retry_after) do
      def initialize(max: nil, base_ms: nil, max_ms: nil, jitter: nil, honor_retry_after: nil) = super

      # From nil, a Hash or RetryOptions.
      def self.from(value)
        case value
        when nil then new
        when RetryOptions then value
        when Hash then new(**value.transform_keys(&:to_sym).slice(:max, :base_ms, :max_ms, :jitter, :honor_retry_after))
        else raise ConfigError, "retries must be a Hash or Tungsten::Runtime::RetryOptions"
        end
      end
    end

    # Client options (every keyword optional):
    #
    # - `redirects`: `"same_origin_read"` or `"none"`; `"none"` in either the API or options wins;
    # - `base_url`: defaults to the API's first server;
    # - `auth`: credentials by auth scheme name: a String for bearer, API key
    #   and basic (`"user:password"`) schemes, a Hash for a composite profile
    #   (by cookie name or config key), for OAuth2 client credentials
    #   (`client_id`, `client_secret`) or the authorization-code flow
    #   (`flow: "authorizationCode"`, `client_id`, `client_secret`,
    #   `redirect_uri`);
    # - `token_store`: where authorization-code tokens live (memory by default);
    # - `timeout_ms`: the per-attempt timeout (30 s);
    # - `retries`: RetryOptions over the API's per-tier defaults;
    # - `idempotency_store`: anything with `get(scope, id)` and `put(scope, id,
    #   key)` (memory by default);
    # - `middleware`: objects with any of `on_request(ctx)`,
    #   `on_response(ctx, response)`, `on_retry(ctx, reason)`;
    # - `validate_responses`: `"warn"` (default), `"strict"` or `"off"`;
    # - `on_diagnostic`: called with warning envelopes;
    # - `headers`: sent on every request;
    # - `max_event_bytes` (1 MiB), `max_collect_bytes` (4 MiB), `max_collect_ms`
    #   (60 s), `max_reconnects` (3), `reconnect_max_ms` (30 s),
    #   `idle_timeout_ms` (none): event streams;
    # - `confirmation_key`: signs confirmation tokens (random per client);
    # - `now` (epoch milliseconds) and `random` (in [0, 1)): callables for tests.
    ClientOptions = Data.define(
      :redirects, :base_url, :auth, :token_store, :timeout_ms, :retries, :idempotency_store, :middleware,
      :validate_responses, :on_diagnostic, :headers, :max_event_bytes, :max_response_bytes, :max_collect_bytes, :max_collect_ms,
      :max_reconnects, :reconnect_max_ms, :idle_timeout_ms, :confirmation_key, :now, :random
    ) do
      def initialize(redirects: nil, base_url: nil, auth: {}, token_store: nil, timeout_ms: nil, retries: nil, idempotency_store: nil,
                     middleware: [], validate_responses: "warn", on_diagnostic: nil, headers: {}, max_event_bytes: nil, max_response_bytes: nil,
                     max_collect_bytes: nil, max_collect_ms: nil, max_reconnects: nil, reconnect_max_ms: nil,
                     idle_timeout_ms: nil, confirmation_key: nil, now: nil, random: nil)
        super
      end

      # Options from nil, a Hash or ClientOptions, with keywords over them.
      def self.from(options = nil, **keywords)
        base =
          case options
          when nil then {}
          when ClientOptions then options.to_h
          when Hash then options.transform_keys(&:to_sym)
          else raise ConfigError, "client options must be a Hash or Tungsten::Runtime::ClientOptions"
          end
        merged = base.merge(keywords.transform_keys(&:to_sym))
        unknown = merged.keys - members
        raise ConfigError, "unknown client option #{unknown.first}" unless unknown.empty?

        new(**merged)
      end
    end

    # Options of one call:
    #
    # - `idempotency_key`: required for caller-owned operations that persist
    #   a key; generate it once per intent and reuse it on every retry;
    # - `confirm`: `true` (a destructive operation only) or the
    #   `confirmation_token` of a preview, bound to the exact arguments;
    # - `refuse_confirm_yes`: `confirm: true` confirms nothing and
    #   remediation never offers it (callers that only forward tokens);
    # - `timeout_ms`: the per-attempt timeout;
    # - `verify`: run the operation's verification hook after success;
    # - `headers`: sent with this call.
    CallOptions = Data.define(:idempotency_key, :confirm, :refuse_confirm_yes, :timeout_ms, :verify, :headers, :max_response_bytes) do
      def initialize(idempotency_key: nil, confirm: nil, refuse_confirm_yes: false, timeout_ms: nil, verify: false,
                     headers: {}, max_response_bytes: nil)
        super
      end

      # Options from nil, a Hash or CallOptions.
      def self.from(value)
        case value
        when nil then new
        when CallOptions then value
        when Hash
          given = value.transform_keys(&:to_sym)
          unknown = given.keys - members
          raise ArgumentError, "unknown call option #{unknown.first}" unless unknown.empty?

          new(**given)
        else raise ArgumentError, "call options must be a Hash or Tungsten::Runtime::CallOptions"
        end
      end

      def key? = idempotency_key.is_a?(String) && !idempotency_key.empty?
      def confirm_yes? = confirm == true
      def token = confirm.is_a?(String) && !confirm.empty? ? confirm : nil

      # Never shows keys or tokens.
      def inspect
        shown = { idempotency_key: key? ? REDACTED : nil, confirm: confirm == true ? true : (token ? REDACTED : nil),
                  timeout_ms: timeout_ms, verify: verify, headers: (headers || {}).keys }
        "#<Tungsten::Runtime::CallOptions #{shown}>"
      end
      alias_method :to_s, :inspect
    end

    # What middleware sees of one attempt; secrets are redacted. `body` is
    # the body as JSON when it is JSON, a text rendering otherwise, UNSET
    # without one.
    RequestContext = Data.define(:operation, :attempt, :http_method, :url, :headers, :body)

    # What middleware sees of an answer.
    ResponseContext = Data.define(:status, :headers)
  end
end
