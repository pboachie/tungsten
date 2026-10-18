# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # The engine behind every generated client. Each call runs one pipeline:
    # pre-flight validation of the arguments (nothing is sent on failure),
    # auth, idempotency keys, the confirmation rules of destructive and
    # irreversible operations, sending with tier-aware retries, and the
    # classification of the answer (or of a lost request) into Ok or Err
    # with the diagnostic envelope. On top of it: previews with
    # confirmation tokens, verification hooks, pagination, polling, macros
    # and server-sent event streams. It is safe for concurrent use; nothing
    # raises for an API or transport failure.
    class ClientCore
      # Same-origin redirects one read follows.
      MAX_REDIRECTS = 5

      # Generated clients require this capability when redirects are forbidden.
      def self.require_redirect_policy_v1; end
      DEFAULT_POLL_INTERVAL_MS = 1000
      DEFAULT_VERIFY_BUDGET_MS = 30_000
      RETRYABLE_CATEGORIES = %w[RATE_LIMITED UPSTREAM_UNAVAILABLE TRANSPORT_FAILED OUTCOME_UNKNOWN].freeze

      # A request ready to send, with its redacted rendering.
      Prepared = Struct.new(:op, :args, :http_method, :url, :display_url, :headers, :body, :key, :key_header, :auth_query,
                            :hidden_query, :secrets, :oauth, :final_url, keyword_init: true)

      attr_reader :descriptors, :base_url, :timeout_ms, :max_event_bytes, :max_collect_bytes, :max_collect_ms,
                  :max_reconnects, :reconnect_max_ms, :idle_timeout_ms

      # @param descriptors [Descriptors] the SDK descriptor document
      # @param options [ClientOptions, Hash, nil]
      def initialize(descriptors, options = nil)
        raise ConfigError, "the SDK descriptor document is missing" unless descriptors.is_a?(Descriptors)

        o = options.is_a?(ClientOptions) ? options : ClientOptions.from(options)
        @descriptors = descriptors
        @api = descriptors.api
        policies = [@api["redirects"], o.redirects]
        unless policies.all? { |policy| policy.nil? || %w[same_origin_read none].include?(policy) }
          raise ConfigError, "redirects must be same_origin_read or none"
        end
        @redirects_disabled = policies.include?("none")
        base = o.base_url.to_s
        base = (@api["servers"] || []).first.to_s if base.empty?
        if !base.empty? && !valid_base_url?(base)
          raise ConfigError, "the base URL is not a valid absolute http(s) URL without credentials"
        end

        @base_url = base
        @auth = (o.auth || {}).to_h.transform_keys(&:to_s)
        @headers = (o.headers || {}).to_h.transform_keys(&:to_s)
        @timeout_ms = positive(o.timeout_ms) || DEFAULT_TIMEOUT_MS
        @max_event_bytes = positive(o.max_event_bytes) || DEFAULT_MAX_EVENT_BYTES
        @max_response_bytes = o.max_response_bytes
        @max_collect_bytes = positive(o.max_collect_bytes) || DEFAULT_MAX_COLLECT_BYTES
        @max_collect_ms = positive(o.max_collect_ms) || DEFAULT_MAX_COLLECT_MS
        @max_reconnects = o.max_reconnects.nil? ? DEFAULT_MAX_RECONNECTS : o.max_reconnects.to_i.clamp(0, 1000)
        @reconnect_max_ms = positive(o.reconnect_max_ms) || DEFAULT_RECONNECT_MAX_MS
        @idle_timeout_ms = positive(o.idle_timeout_ms) ? [o.idle_timeout_ms.to_i, 1].max : nil
        @retries = RetryOptions.from(o.retries)
        @store = o.idempotency_store || MemoryIdempotencyStore.new
        @middleware = Array(o.middleware)
        @validate_responses = %w[warn strict off].include?(o.validate_responses.to_s) ? o.validate_responses.to_s : "warn"
        @on_diagnostic = o.on_diagnostic
        key = o.confirmation_key
        copied_key = String.new(key, encoding: Encoding::BINARY) if key.is_a?(String)
        if !key.nil? && (copied_key.nil? || copied_key.bytesize < 32)
          raise ConfigError, "confirmation_key must contain at least 32 bytes; omit it to generate a random key"
        end
        @confirmation_key = copied_key || SecureRandom.random_bytes(32)
        @now_fn = o.now
        @random_fn = o.random
        @token_store = o.token_store || MemoryTokenStore.new
        @lock = Mutex.new
        @key_lock = Mutex.new
        @cc_lock = Mutex.new
        @cc_tokens = {}
        @used_tokens = {}
        @oauth_gates = {}
        @registry = descriptors.operations.dup
      end

      # The API descriptor (the document's `api`).
      def api = @api

      # A registered operation by id.
      def operation(id) = @lock.synchronize { @registry[id] }

      # Make operations resolvable by id (verification hooks, endpoint
      # previews, macro steps).
      def register(*ops)
        @lock.synchronize { ops.each { |op| @registry[op.id] ||= op if op.is_a?(OperationDescriptor) && !op.id.empty? } }
      end

      # Call an operation (an id or a descriptor) with an arguments Hash
      # keyed by the SDK's argument names.
      #
      # @return [Ok, Err]
      def call(op, args = {}, opts = nil)
        with_options(op, opts) { |d, o| call_with(d, args, o, nil, nil) }
      end

      # Render the request without sending it (or run the operation's header
      # or endpoint preview). A mutating operation gets a confirmation token
      # bound to these exact arguments for five minutes.
      #
      # @return [Ok, Err] `value` is a PreviewResult
      def preview(op, args = {}, opts = nil)
        with_options(op, opts) { |d, o| preview_with(d, args, o) }
      end

      # Call a read operation until `until_` holds or the budget runs out;
      # `value` is {"body" => ..., "timed_out" => bool}.
      def poll(op, args = {}, until_: nil, interval_ms: DEFAULT_POLL_INTERVAL_MS, budget_ms: DEFAULT_VERIFY_BUDGET_MS, opts: nil)
        with_options(op, opts) { |d, o| poll_with(d, args, until_, interval_ms, budget_ms, o, nil) }
      end

      # Milliseconds since the epoch (the clock of ClientOptions `now`).
      def now
        v = @now_fn&.call
        v.is_a?(Numeric) ? v.to_i : (Time.now.to_f * 1000).to_i
      end

      private

      def positive(value) = value.is_a?(Numeric) && value.positive? ? value.to_i : nil

      def valid_base_url?(base)
        u = URI.parse(base)
        %w[http https].include?(u.scheme) && !u.host.to_s.empty? && u.userinfo.nil?
      rescue URI::Error
        false
      end

      # The descriptor of an id or descriptor and the call options, or the
      # Err that explains why there is none.
      def with_options(op, opts)
        d = op.is_a?(OperationDescriptor) ? op : operation(op.to_s)
        unless d
          id = op.to_s.empty? ? "<unknown operation>" : op.to_s
          return Runtime.diag(id, "VALIDATION_FAILED").param("operation").expected("an operation id of this SDK")
                        .remedy("This SDK has no operation #{id}; check the id. Nothing was sent.").err
        end
        begin
          o = CallOptions.from(opts)
        rescue ArgumentError => e
          return Runtime.diag(d.id, "VALIDATION_FAILED").param("opts").expected("call options")
                        .remedy("Pass the call options as a Hash of idempotency_key, confirm, timeout_ms, verify and headers (#{e.message}).").err
        end
        yield d, o
      end

      def random
        v = @random_fn ? @random_fn.call : SecureRandom.random_number
        v.is_a?(Numeric) && v.finite? ? v.to_f.clamp(0.0, 1.0) : 0.5
      end

      # The retry policy of one operation: the runtime defaults, then the
      # API's defaults for the operation's tier, then ClientOptions
      # `retries`, field by field.
      def retry_options(op)
        out = { max: 3, base_ms: 200.0, max_ms: 5000.0, jitter: "full", honor_retry_after: true }
        layers = []
        if (tiers = @api["retries"])
          layers << tiers[op.mutation? ? "mutating" : "read_only"]
        end
        layers << @retries.to_h.transform_keys(&:to_s)
        layers.compact.each do |l|
          out[:max] = l["max"].to_i.clamp(0, 100) unless l["max"].nil?
          out[:base_ms] = l["base_ms"].to_f unless l["base_ms"].nil?
          out[:max_ms] = l["max_ms"].to_f unless l["max_ms"].nil?
          out[:jitter] = l["jitter"].to_s unless l["jitter"].nil? || l["jitter"].to_s.empty?
          out[:honor_retry_after] = l["honor_retry_after"] == true unless l["honor_retry_after"].nil?
        end
        out
      end

      # The per-attempt timeout (at least one millisecond).
      def attempt_timeout(opts)
        t = opts.timeout_ms.is_a?(Numeric) && opts.timeout_ms.positive? ? opts.timeout_ms.to_i : @timeout_ms
        [t, 1].max
      end

      # Report a warning; a failing observer never changes the call.
      def emit(diagnostic)
        @on_diagnostic&.call(diagnostic)
      rescue StandardError
        nil
      end

      def hook(name, *args)
        @middleware.each do |m|
          m.public_send(name, *args) if m.respond_to?(name)
        rescue StandardError
          nil
        end
      end

      # The pipeline shared by calls, pages, polls and macro steps.
      #
      # With `page_state` (a link-header pagination) a request URL used
      # before is refused unsent, and the URL that answered is recorded.
      def call_with(op, args, opts, url_override, claim, page_state = nil)
        prepared = prepare(op, args, opts, :call, url_override, claim)
        return prepared if prepared.is_a?(Err)

        if page_state && !page_state.request_urls.add?(prepared.url)
          return Runtime.diag(op.id, "UNEXPECTED_RESPONSE")
                        .remedy("Pagination detected a repeated effective page request URL after authentication. Fix the server's next-page Link before continuing; no repeated request was sent.").err
        end

        result = send_request(prepared, opts)
        if result.is_a?(Err) && result.error.http_status == 401 && prepared.oauth && !(result.error.category == "UNEXPECTED_RESPONSE" && result.error.code == "RESPONSE_TOO_LARGE")
          result = resend_rejected(prepared, result, opts)
        end
        return result if result.is_a?(Err)

        if page_state
          page_state.last_url = prepared.final_url
          page_state.request_urls << prepared.final_url if prepared.final_url
        end

        if opts.verify && op.agent["verify"]
          return result.with(verification: verify(op, prepared.args, result.raw.nil? ? UNSET : result.raw, opts))
        end
        result
      end

      # Send a request whose stored OAuth2 token was rejected (401) once more
      # with the refreshed token.
      def resend_rejected(p, rejected, opts)
        scheme = p.oauth
        p.oauth = nil
        current = p.headers.get("Authorization")
        return rejected unless current&.start_with?("Bearer ")

        token = refresh_after_rejection(scheme, current.delete_prefix("Bearer "))
        return rejected unless token

        p.headers.set("Authorization", "Bearer #{token}", true)
        p.secrets << token
        send_request(p, opts)
      end

      def validation(op, path, value, expected, remediation)
        received = Util.envelope_value(UNSET.equal?(value) ? nil : Helpers.redact_below(op, path, value), Helpers.sensitive_arg?(op, path))
        Runtime.diag(op.id, "VALIDATION_FAILED").param(Helpers.argument_path(op, path)).received(received)
               .expected(expected).remedy(remediation).err
      end

      def validate_args(op, args, placeholders)
        params = op.argument_params
        params.each do |p|
          next unless p.required && (!args.key?(p.name) || args[p.name].nil?)

          return validation(op, [p.name], args.fetch(p.name, UNSET),
                            "a value for the required #{p.location} parameter #{p.wire}",
                            "Pass #{p.name}; #{op.id} cannot be called without it.")
        end
        body = op.body
        if body&.required && body.arg && !args.key?(body.arg)
          return validation(op, [body.arg], UNSET, "a request body", "Pass the request body as #{body.arg}.")
        end
        if op.request
          issues =
            begin
              op.request.validate(args)
            rescue StandardError
              return Runtime.diag(op.id, "UNEXPECTED_RESPONSE").retrying("never")
                            .remedy("The SDK failed while building or sending the request (the request validator failed). This is a bug in the generated SDK or the runtime; report it. Nothing was sent.").err
            end
          return args if issues.empty?

          issue =
            if placeholders
              issues.find do |i|
                (1..i.path.length).none? { |n| Helpers.placeholder?(Util.get_path(args, i.path.first(n))) }
              end
            else
              issues.first
            end
          return args if issue.nil?

          shown = Helpers.argument_path(op, issue.path)
          return validation(op, issue.path, Util.get_path(args, issue.path), issue.message,
                            "Fix #{shown} (#{issue.message}) and call again.")
        end
        return args if op.rpc && body.nil?

        allowed = params.map(&:name)
        allowed += body.arg ? [body.arg] : body.fields.map(&:first) if body
        args.each_key do |k|
          next if allowed.include?(k)

          names = allowed.sort.first(20)
          list = names.empty? ? "no arguments" : names.join(", ")
          return validation(op, [k], args[k], "one of: #{list}", "Remove #{k}; #{op.id} does not accept it.")
        end
        args
      end

      def check_key(op, opts, purpose)
        policy = op.agent["idempotency"]
        note = policy["note"].to_s.empty? ? "" : " #{policy['note']}"
        unless opts.key?
          if purpose == :call && policy["policy"] == "caller_owned" && policy["persist_required"]
            fresh = policy["format"].to_s.downcase.include?("uuid") ? "UUIDv4" : "unique key"
            return Runtime.diag(op.id, "VALIDATION_FAILED").param("idempotency_key")
                          .expected(Keys.format_description(policy["format"]))
                          .remedy("Generate a random #{fresh}, persist it with the intent of this #{op.id} call, and pass it as idempotency_key. " \
                                  "Reuse the exact same key on every retry; a new key can apply the effect twice.#{note}").err
          end
          return nil
        end
        return nil if policy["policy"] == "none" && op.params.none? { |p| p.role == "idempotency_key" }
        return nil if %w[content_identity content_hash].include?(policy["policy"])

        format = policy["policy"] == "caller_owned" ? policy["format"] : nil
        expected = Keys.check_format(opts.idempotency_key, format)
        return nil unless expected

        Runtime.diag(op.id, "VALIDATION_FAILED").param("idempotency_key")
               .received(Util.envelope_value(opts.idempotency_key, false)).expected(expected)
               .remedy("Pass idempotency_key as #{expected}. Generate it once, persist it with the intent of this call, and reuse it on every retry.#{note}").err
      end

      # The confirmation rule of a tier: destructive accepts `confirm: true`
      # (unless refused) or a token, irreversible only a token issued for
      # the subject and these exact arguments.
      def confirmed(id, subject, preview_call, safety, args, opts)
        return nil unless %w[destructive irreversible].include?(safety)

        allow_yes = !opts.refuse_confirm_yes
        how = "call #{preview_call} with the same arguments and pass its confirmation_token as confirm"
        required = lambda do |remediation|
          Runtime.diag(id, "CONFIRMATION_REQUIRED").param("confirm").expected("a confirmation_token from preview()")
                 .remedy(remediation).err
        end
        if opts.confirm_yes?
          return nil if safety == "destructive" && allow_yes

          return required.call("#{id} is #{safety}, so confirm: true is not accepted: #{how}.")
        end
        if opts.token
          case Keys.check_token(@confirmation_key, opts.token, subject, args, now)
          when :valid then return nil
          when :expired
            return required.call("The confirmation token expired (tokens last #{Keys::TTL_MS / 60_000} minutes): #{how}.")
          else
            return required.call("The confirmation token was not issued by this client for #{id} with exactly these arguments: #{how}.")
          end
        end
        or_yes = safety == "destructive" && allow_yes ? " (or pass confirm: true)" : ""
        required.call("#{id} is #{safety}: #{how}#{or_yes}.")
      end

      # Spend a valid token on the call about to be sent: its first use binds
      # it to that call's replay protection (`bind`, nil for none), and a
      # later use is accepted only as a retry under the same protection.
      def claim_token(id, token, bind, preview_call)
        t = now
        @lock.synchronize do
          @used_tokens.delete_if { |_, u| u[:expiry] <= t }
          if (previous = @used_tokens[token])
            return nil if previous[:bind] && bind && previous[:bind] == bind

            why =
              if previous[:bind].nil? then "This call has no idempotency key, so a repeat can apply the effect twice"
              elsif bind.nil? then "It was used with an idempotency key; only a retry with that same key may reuse it"
              else "It was used with another idempotency key; only a retry with that same key may reuse it"
              end
            return Runtime.diag(id, "CONFIRMATION_REQUIRED").param("confirm").expected("a confirmation_token from preview()")
                          .remedy("This confirmation token was already used for one #{id} call. #{why}. Check whether that call took effect; " \
                                  "to send again, call #{preview_call} and confirm with its new token.").err
          end
          parsed = Keys.token_expiry(token)
          @used_tokens[token] = { bind: bind, expiry: parsed ? parsed[0] : t + Keys::TTL_MS }
        end
        nil
      end

      # Arguments as a JSON object, or the Err of a value that is not one.
      def normalize_args(op, raw)
        value =
          begin
            Json.from_ruby(raw.nil? ? {} : raw)
          rescue ArgumentError
            UNSET
          end
        return value if value.is_a?(Hash)

        names = op.argument_params.first(3).map { |p| "#{p.name}: ..." }
        Runtime.diag(op.id, "VALIDATION_FAILED").param("args").received(Util.envelope_value(value, false))
               .expected("an object of named arguments")
               .remedy("Pass the arguments of #{op.id} as one object, e.g. {#{names.join(', ')}}.").err
      end

      def descriptor_err(op)
        problem = Helpers.descriptor_problem(op)
        return nil unless problem

        id = op.id.empty? ? "<unknown operation>" : op.id
        Runtime.diag(id, "VALIDATION_FAILED").param("operation").expected("a valid operation descriptor")
               .remedy("The operation descriptor is invalid: #{problem}. Regenerate the SDK or fix the descriptor; nothing was sent.").err
      end

      # Validate, authenticate and render a request. `purpose` is :call,
      # :preview, :server_preview or :macro_preview.
      def prepare(op, raw_args, opts, purpose, url_override, claim)
        if (err = descriptor_err(op))
          return err
        end

        register(op)
        args = normalize_args(op, raw_args)
        return args if args.is_a?(Err)
        if Util.nests_deeper?(args, Util::MAX_ARG_DEPTH)
          return validation(op, [], "<deeply nested value>", "a JSON value nested at most #{Util::MAX_ARG_DEPTH} levels",
                            "Pass arguments without such deep nesting.")
        end

        args = validate_args(op, args, purpose == :macro_preview)
        return args if args.is_a?(Err)

        err = check_key(op, opts, purpose)
        return err if err

        if purpose == :call && claim.nil?
          err = confirmed(op.id, op.id, "preview(...)", op.agent["safety"], args, opts)
          return err if err
        end
        method = op.http_method
        auth = resolve_auth(op, method)
        return Runtime.diag(op.id, "AUTH_FAILED").remedy(auth.failed).err if auth.failed

        secrets = auth.secrets
        Helpers.sensitive_request_paths(op).each do |path|
          found = []
          Helpers.values_at(args, Util.split_path(path), found)
          found.each { |v| secrets << v }
        end
        urls = build_url(op, args, auth.plan, url_override)
        return urls if urls.is_a?(Err)

        full_url, display_url = urls
        headers = HeaderBag.new
        build_headers(op, args, opts, auth.plan, headers)
        param_names = op.argument_params.map(&:name)
        body_paths = Helpers.sensitive_body_paths(op)
        value = UNSET
        value = Serialize.body_value(op, args, param_names) if !safe_method?(method) || method == "OPTIONS"
        redact = lambda do |display|
          next REDACTED if body_paths.include?("")

          Util.redact_sensitive_keys(Util.redact_paths(display, body_paths))
        end
        encoded = Serialize.encode_body(op.body, value, redact)
        if encoded.is_a?(Serialize::SerializationError)
          path = op.body&.arg && encoded.parameter == "body" ? [op.body.arg] : []
          return validation(op, path, encoded.value, encoded.expected, "Pass #{encoded.parameter} as #{encoded.expected}.")
        end
        headers.set("Content-Type", encoded.content_type, false) if encoded.content_type
        header = Keys.header(op)
        key = idempotency_key(op, args, opts, purpose, encoded)
        return key if key.is_a?(Err)

        policy = op.agent["idempotency"]["policy"]
        if key
          headers.set(header, key, true)
          secrets << key
        elsif purpose != :call && %w[auto caller_owned].include?(policy)
          headers.set(header, "<set at call time>", false)
        end
        auth.plan.headers.each { |name, v, secret| headers.set(name, v, secret) }
        if purpose == :server_preview && op.agent["preview"]["mode"] == "header"
          headers.set(op.agent["preview"]["header"], op.agent["preview"]["value"], false)
        end
        headers.entries.each do |e|
          next if Serialize.valid_header_name?(e.name) && Serialize.valid_header_value?(e.value)

          if auth.plan.headers.any? { |name, _, _| name.casecmp?(e.name) }
            return Runtime.diag(op.id, "AUTH_FAILED")
                          .remedy("The credential for header #{e.name} contains characters not allowed in an HTTP header; check ClientOptions.auth.").err
          end
          p = op.params.find { |x| x.wire.casecmp?(e.name) }
          return validation(op, p ? [p.name] : [], e.secret ? REDACTED : e.value,
                            "a header value of visible ASCII characters (no line breaks)",
                            "Header #{e.name} cannot carry this value; remove line breaks and non-Latin-1 characters.")
        end
        if purpose == :call
          if claim
            err = claim.claim(self)
            return err if err
          elsif opts.token && %w[destructive irreversible].include?(op.agent["safety"])
            bind = key || (policy == "content_identity" ? "content-identity" : nil)
            err = claim_token(op.id, opts.token, bind, "preview(...)")
            return err if err
          end
        end
        headers.secrets.each { |s| secrets << s }
        hidden = auth.plan.query.map(&:first)
        op.argument_params.each do |p|
          next unless p.location == "query" && args.key?(p.name)
          next if request_display_value(op, p, args[p.name]) == args[p.name]

          hidden.concat(Serialize.query_param(p, args[p.name]).map { |part| query_pairs(part).first&.first }.compact)
        end
        Prepared.new(op: op, args: args, http_method: method, url: full_url, display_url: display_url, headers: headers,
                     body: encoded, key: key, key_header: header, auth_query: auth.plan.query, hidden_query: hidden,
                     secrets: secrets, oauth: auth.oauth)
      end

      def bad_base(op)
        Runtime.diag(op.id, "TRANSPORT_FAILED").retrying("never")
               .remedy("The base URL is not a valid absolute http(s) URL without credentials: fix ClientOptions.base_url. Nothing was sent.").err
      end

      def dot_segment?(segment)
        rest = segment.downcase
        count = 0
        while count < 3
          if rest.start_with?("%2e") then rest = rest[3..]
          elsif rest.start_with?(".") then rest = rest[1..]
          else break
          end
          count += 1
        end
        segment.empty? || (rest.empty? && count.between?(1, 2))
      end

      def request_display_value(op, param, value)
        return REDACTED if Helpers.sensitive_arg?(op, [param.name]) || Util.looks_sensitive?(param.wire)

        Util.redact_sensitive_keys(Helpers.redact_below(op, [param.name], value))
      end

      # Redirects and pagination can supply paths other than the template.
      # Hide the path conservatively when it may carry sensitive arguments.
      def display_override_url(op, args, url)
        uri = URI.parse(url)
        hidden = []
        op.argument_params.each do |p|
          next unless args.key?(p.name)
          next if request_display_value(op, p, args[p.name]) == args[p.name]

          if p.location == "path"
            uri.path = "/#{Serialize.encode_component(REDACTED)}"
          elsif p.location == "query"
            hidden.concat(Serialize.query_param(p, args[p.name]).flat_map { |part| query_pairs(part).map(&:first) })
          end
        end
        with_query(uri.to_s, [], hidden, replace: true)
      rescue URI::Error
        url # The caller rejects invalid URLs before sending.
      end

      # [url, display url], or an Err.
      def build_url(op, args, plan, url_override)
        if @base_url.empty? && url_override.nil?
          return Runtime.diag(op.id, "TRANSPORT_FAILED").retrying("never")
                        .remedy("No base URL is configured: set ClientOptions.base_url. Nothing was sent.").err
        end
        params = op.argument_params
        if url_override
          full = url_override
          display = display_override_url(op, args, url_override)
          unless plan.query.empty?
            begin
              full = with_query(full, plan.query, [], replace: false)
              display = with_query(display, plan.query.map { |k, _| [k, REDACTED] }, [], replace: true)
            rescue URI::Error
              return bad_base(op)
            end
          end
        else
          unknown = nil
          missing = nil
          displayed_path = {}
          built = op.path.gsub(/\{([^{}]+)\}/) do
            inner = Regexp.last_match(1)
            p = params.find { |x| x.location == "path" && x.wire == inner } || params.find { |x| x.location == "path" && x.name == inner }
            if p.nil?
              unknown ||= inner
              "{#{inner}}"
            elsif !args.key?(p.name) || args[p.name].nil?
              missing ||= p
              "{#{inner}}"
            else
              displayed_path[inner] = Serialize.path_param(p, request_display_value(op, p, args[p.name]))
              Serialize.path_param(p, args[p.name])
            end
          end
          if unknown
            return Runtime.diag(op.id, "VALIDATION_FAILED").param("operation").expected("a valid operation descriptor")
                          .remedy("The path template has a placeholder {#{unknown}} with no path parameter; regenerate the SDK.").err
          end
          return validation(op, [missing.name], UNSET, "a value for path parameter #{missing.wire}", "Pass #{missing.name}.") if missing

          template = op.path.split("/", -1)
          segments = built.split("/", -1)
          if template.length == segments.length
            segments.each_with_index do |segment, i|
              tmpl = template[i]
              next unless tmpl.include?("{") && dot_segment?(segment)

              wire = tmpl[/\{([^}]*)\}/, 1]
              p = params.find { |x| x.location == "path" && (x.wire == wire || x.name == wire) }
              return validation(op, p ? [p.name] : [], p ? args[p.name] : segment,
                                "a non-empty path segment other than . and ..",
                                "Pass #{p ? p.name : 'the path parameter'} as the identifier of one resource; \"#{segment}\" would change the request path.")
            end
          end
          parts = []
          shown = []
          params.each do |p|
            next unless p.location == "query" && args.key?(p.name)

            serialized = Serialize.query_param(p, args[p.name])
            shown.concat(Serialize.query_param(p, request_display_value(op, p, args[p.name])))
            parts.concat(serialized)
          end
          plan.query.each do |k, v|
            parts << "#{Serialize.encode_component(k)}=#{Serialize.encode_component(v)}"
            shown << "#{Serialize.encode_component(k)}=#{REDACTED}"
          end
          root = @base_url.sub(%r{/+\z}, "")
          join = ->(list) { list.empty? ? "" : (built.include?("?") ? "&" : "?") + list.join("&") }
          full = root + built + join.call(parts)
          displayed = op.path.gsub(/\{([^{}]+)\}/) { displayed_path.fetch(Regexp.last_match(1)) }
          display = root + displayed + join.call(shown)
        end
        return bad_base(op) unless valid_base_url?(full)

        [full, display]
      end

      def query_pairs(raw)
        return [] if raw.nil? || raw.empty?

        raw.split("&").reject(&:empty?).map do |part|
          k, v = part.split("=", 2)
          [URI.decode_www_form_component(k.to_s), URI.decode_www_form_component(v.to_s)]
        rescue ArgumentError
          [k.to_s, v.to_s]
        end
      end

      # A URL with the pairs set (URLSearchParams.set when `replace`, else
      # only when absent) and the `hidden` parameters shown redacted.
      def with_query(url, pairs, hidden, replace:)
        u = URI.parse(url)
        list = query_pairs(u.query)
        pairs.each do |k, v|
          index = list.index { |name, _| name == k }
          if index.nil?
            list << [k, v]
          elsif replace
            list[index] = [k, v]
            list = list.each_with_index.reject { |(name, _), i| name == k && i != index }.map(&:first)
          end
        end
        hidden.each do |name|
          list = list.map { |k, v| k == name ? [k, REDACTED] : [k, v] }
        end
        u.query = list.empty? ? nil : list.map { |k, v| "#{Serialize.encode_form_component(k)}=#{Serialize.encode_form_component(v)}" }.join("&")
        u.to_s
      end

      def build_headers(op, args, opts, plan, headers)
        accept = []
        op.responses.each do |r|
          accept << r.media_type if r.kind == "success" && r.media_type.is_a?(String) && !r.media_type.empty? && !accept.include?(r.media_type)
        end
        headers.set("Accept", accept.join(", "), false) unless accept.empty?
        api_name = @api["name"].to_s.empty? ? "api" : @api["name"]
        api_version = @api["version"].to_s.empty? ? "0" : @api["version"]
        headers.set("X-Tungsten-Runtime", "tungsten-rb/#{VERSION} #{api_name}-sdk/#{api_version}", false)
        headers.set("X-Tungsten-Operation", op.id, false)
        tungsten = @api["tungsten_version"].to_s.empty? ? VERSION : @api["tungsten_version"]
        headers.set("User-Agent", "#{api_name}-sdk/#{api_version} tungsten/#{tungsten} (ruby)", false)
        op.params.each do |p|
          headers.set(p.wire, p.constant.to_s, false) if p.role == "constant" && p.location == "header" && p.constant
        end
        [@headers, (opts.headers || {}).to_h.transform_keys(&:to_s)].each do |extra|
          extra.keys.sort.each { |n| headers.set(n, extra[n].to_s, Util.looks_sensitive?(n)) }
        end
        cookies = []
        existing = headers.get("Cookie")
        cookies << existing if existing
        op.argument_params.each do |p|
          next unless args.key?(p.name) && !args[p.name].nil?

          case p.location
          when "header"
            secret = request_display_value(op, p, args[p.name]) != args[p.name]
            headers.set(p.wire, Serialize.header_param(p, args[p.name]), secret)
          when "cookie" then cookies << Serialize.cookie_param(p, args[p.name])
          end
        end
        plan.cookies.each { |k, v| cookies << "#{k}=#{v}" }
        # Cookies are session material: the whole header is always redacted.
        headers.set("Cookie", cookies.join("; "), true) unless cookies.empty?
      end

      # The key to send (nil for none), or an Err.
      def idempotency_key(op, args, opts, purpose, encoded)
        supplied = opts.key? ? opts.idempotency_key : nil
        case op.agent["idempotency"]["policy"]
        when "content_identity" then nil
        when "content_hash" then encoded.hashed ? Util.sha256_hex(encoded.hashed) : nil
        when "caller_owned" then supplied
        when "auto"
          return supplied if supplied
          return nil unless purpose == :call

          logical = Util.sha256_hex(Json.canonical(args))
          @key_lock.synchronize do
            if @store.respond_to?(:reserve)
              key = @store.reserve(op.id, logical, Util.uuid_v4)
              return key if key.is_a?(String) && !key.empty?
            else
              existing = @store.get(op.id, logical)
              return existing if existing.is_a?(String) && !existing.empty?

              key = Util.uuid_v4
              @store.put(op.id, logical, key)
              return key if @store.get(op.id, logical) == key
            end

            Runtime.diag(op.id, "TRANSPORT_FAILED").retrying("never")
                   .remedy("The idempotency store failed (the key could not be stored), so the call was not sent rather than risk a second key " \
                           "for the same intent. Fix or replace ClientOptions.idempotency_store.").err
          end
        else
          op.params.any? { |p| p.role == "idempotency_key" } ? supplied : nil
        end
      rescue StandardError
        Runtime.diag(op.id, "TRANSPORT_FAILED").retrying("never")
               .remedy("The idempotency store failed (it raised an error), so the call was not sent rather than risk a second key " \
                       "for the same intent. Fix or replace ClientOptions.idempotency_store.").err
      end

      # ------------------------------------------------------------ sending

      def request_context(p, attempt)
        RequestContext.new(operation: p.op.id, attempt: attempt, http_method: p.http_method, url: p.display_url,
                           headers: p.headers.to_h(true), body: p.body.empty ? UNSET : p.body.display)
      end

      def send_request(p, opts)
        out = send_with(p, opts, false)
        return out unless out.is_a?(Transport::Outcome)

        out.stream.close
        Runtime.diag(p.op.id, "UNEXPECTED_RESPONSE").retrying("never")
               .remedy("The SDK received an event stream it did not ask for. This is a bug in the runtime; report it. If the call may have been sent, check its effect before repeating it.").err
      end

      def redirect?(status) = [301, 302, 303, 307, 308].include?(status)

      def default_port(scheme) = { "http" => 80, "https" => 443 }[scheme.to_s.downcase]

      def same_origin?(a, b)
        a.scheme.to_s.casecmp?(b.scheme.to_s) && a.host.to_s.casecmp?(b.host.to_s) &&
          (a.port || default_port(a.scheme)) == (b.port || default_port(b.scheme))
      end

      # [url, shown url] of a read's redirect that stays on the request's
      # origin, with the auth query re-applied; nil otherwise.
      def redirect_target(p, from, location)
        return nil if location.nil? || location.empty?

        base = URI.parse(from)
        target = base.merge(location)
        return nil unless same_origin?(base, target) && target.userinfo.nil?

        sent = with_query(target.to_s, p.auth_query, [], replace: false)
        display = display_override_url(p.op, p.args, sent)
        [sent, with_query(display, [], p.hidden_query, replace: true)]
      rescue URI::Error
        nil
      end

      # Send with retries. With `stream`, a 2xx text/event-stream answer is
      # returned unread as [:stream, meta, body]; otherwise Ok or Err.
      def send_with(p, opts, stream)
        op = p.op
        cap = opts.max_response_bytes.nil? ? @max_response_bytes : opts.max_response_bytes
        unless cap.nil? || (cap.is_a?(Integer) && cap.between?(1, 9_007_199_254_740_991))
          return Runtime.diag(op.id, "VALIDATION_FAILED").param("maxResponseBytes")
                        .remedy("maxResponseBytes must be a positive safe integer; nothing was sent.").err
        end
        retries = retry_options(op)
        mutation = op.mutation?
        protected_ = Classify.replay_protection?(op, !p.key.nil?)
        check = mutation && !protected_ ? outcome_check(op, p.args) : nil
        timeout = attempt_timeout(opts)
        attempts = 0
        loop do
          attempts += 1
          rctx = request_context(p, attempts)
          hook(:on_request, rctx)
          target = p.url
          method = p.http_method
          body = p.body
          headers = p.headers.entries.map { |e| [e.name, e.value] }
          send_once = lambda do
            Transport.attempt(url: target, method: method, headers: headers, body: body, timeout_ms: timeout, stream: stream,
                              idle_ms: stream ? @idle_timeout_ms : nil, max_response_bytes: cap)
          end
          outcome = send_once.call
          unless mutation || @redirects_disabled
            MAX_REDIRECTS.times do
              break unless outcome.kind == :response && outcome.body_failure != "oversized" && redirect?(outcome.status)

              next_url = redirect_target(p, target, outcome.headers["location"])
              break unless next_url

              target = next_url[0]
              if ![307, 308].include?(outcome.status) && !%w[GET HEAD].include?(method)
                method = "GET"
                body = Serialize::EMPTY
                headers = headers.reject { |name, _| name.casecmp?("content-type") }
              end
              rctx = rctx.with(url: next_url[1], http_method: method)
              outcome = send_once.call
            end
          end
          p.final_url = target
          ctx = Classify::CallContext.new(api: @api, op: op, key: p.key, key_header: p.key_header, attempts: attempts, check: check)
          result = classify_response(ctx, p, outcome, rctx, timeout)
          return result unless result.is_a?(Err)

          d = Util.scrub_diagnostic(result.error, p.secrets)
          done = Err.new(error: d, partial: result.partial)
          return done if attempts > retries[:max] || !RETRYABLE_CATEGORIES.include?(d.category)
          return done if %w[never after_remediation].include?(d.retryable) || (mutation && !protected_)

          if d.retry_after_ms && retries[:honor_retry_after]
            return done if d.retry_after_ms > retries[:max_ms]

            delay = d.retry_after_ms.to_f
          else
            ceiling = [retries[:max_ms], retries[:base_ms] * (2**(attempts - 1))].min
            r = random
            delay =
              case retries[:jitter]
              when "none" then ceiling
              when "equal" then (ceiling / 2) + (r * ceiling / 2)
              else r * ceiling
              end
          end
          hook(:on_retry, rctx, d)
          sleep([delay, 0].max / 1000.0)
        end
      end

      def classify_response(ctx, p, outcome, rctx, timeout)
        op = p.op
        mutation = op.mutation?
        attempts = ctx.attempts
        case outcome.kind
        when :not_sent
          return Runtime.diag(op.id, "TRANSPORT_FAILED")
                        .remedy("The request could not be delivered (#{outcome.detail}), so the server did not receive it. Check base_url and network access, then call again.")
                        .attempts(attempts).err
        when :lost
          cause = "The connection failed before a response arrived (#{outcome.detail})."
          return Err.new(error: Classify.outcome_unknown(ctx, cause)) if mutation

          return Runtime.diag(op.id, "TRANSPORT_FAILED").remedy("#{cause} This read has no side effects; call again.").attempts(attempts).err
        when :timeout
          return Err.new(error: Classify.outcome_unknown(ctx, "No response arrived within #{timeout} ms.")) if mutation

          return Runtime.diag(op.id, "UPSTREAM_UNAVAILABLE")
                        .remedy("No response arrived within #{timeout} ms. This read has no side effects; call again later or with a larger timeout_ms.")
                        .attempts(attempts).err
        when :streaming
          hook(:on_response, rctx, ResponseContext.new(status: outcome.status, headers: outcome.headers))
          meta = ResponseMeta.new(status: outcome.status, headers: outcome.headers, request_id: Classify.request_id(outcome.headers), attempts: attempts)
          return [:stream, meta, outcome.stream]
        end
        status = outcome.status
        headers = outcome.headers
        hook(:on_response, rctx, ResponseContext.new(status: status, headers: headers))
        rid = Classify.request_id(headers)
        retry_after = Transport.parse_retry_after(headers["retry-after"], now)
        declared = Classify.match_response(op.responses, status)
        # A status the descriptor declares (exactly or by class) is classified as declared. With
        # redirects disabled, a 3xx it does not declare is a refused redirect, never a success.
        refused_redirect = @redirects_disabled && status.between?(300, 399) && (declared.nil? || declared.status == "default")
        success = !refused_redirect && (status.between?(200, 299) || (declared&.kind == "success" && status.between?(100, 399)))
        verify_op = op.agent["verify"]
        hint = verify_op ? "Call #{verify_op['operation']} to read the current state." : nil
        if outcome.body_failure == "oversized"
          after = mutation ? "The call may have taken effect; inspect API state before retrying." : "The read is incomplete."
          return Runtime.diag(op.id, "UNEXPECTED_RESPONSE").code("RESPONSE_TOO_LARGE").status(status).request_id(rid)
                        .retrying("never").next_action(hint).attempts(attempts)
                        .remedy("Response exceeded maxResponseBytes and was abandoned. #{after}").err
        end
        if success && outcome.body.nil?
          failure = outcome.body_failure == "timeout" ? "timeout" : "broken"
          unless mutation
            return Runtime.diag(op.id, "UPSTREAM_UNAVAILABLE").status(status).request_id(rid).retrying("after_delay")
                          .remedy("The response body was cut off (#{failure}). This read has no side effects; call again.").attempts(attempts).err
          end
          return Runtime.diag(op.id, "UNEXPECTED_RESPONSE").status(status).request_id(rid).retrying("never")
                        .remedy("The server accepted #{op.id} (HTTP #{status}) but the response body was cut off (#{failure}). The call took effect; do not repeat it.")
                        .next_action(hint).attempts(attempts).err
        end
        data = outcome.body || "".b
        declared_media = declared&.media_type.to_s
        unless success
          if status.between?(300, 399)
            to = headers["location"] ? " to #{Json.take_units(headers['location'], 200)}" : ""
            remediation =
              if mutation
                "The server answered with a redirect#{to}. Redirects are never followed on mutations; check base_url (scheme, host, trailing slash). " \
                  "Whether the call took effect is unknown only if the server applied it before redirecting."
              elsif @redirects_disabled
                "The server answered with a redirect#{to}. Redirects are disabled; check redirects and base_url."
              else
                "The server answered with a redirect#{to} that leaves the API's origin (or too many redirects). It is not followed, so credentials never leave the API's host; check base_url."
              end
            return Runtime.diag(op.id, "UNEXPECTED_RESPONSE").status(status).request_id(rid).remedy(remediation)
                          .next_action(mutation ? hint : nil).attempts(attempts).err
          end
          decoded = Classify.decode_body(data, headers, declared_media)
          return Err.new(error: Classify.classify_error(ctx, status, headers, decoded, retry_after))
        end
        decoded = Classify.decode_body(data, headers, declared_media)
        mode = @validate_responses
        after_effect = mutation ? " The call took effect; do not repeat it." : ""
        problem = nil
        if decoded.invalid_json
          expected = decoded.jsonl ? "a JSON Lines body" : "a JSON body"
          remediation =
            if decoded.jsonl
              "The success response announced JSON Lines but line #{decoded.bad_line} did not parse.#{after_effect}"
            else
              "The success response announced JSON but did not parse.#{after_effect}"
            end
          problem = Runtime.diag(op.id, "UNEXPECTED_RESPONSE").status(status).request_id(rid).param("response")
                           .expected(expected).remedy(remediation).attempts(attempts).build
        elsif mode != "off" && op.response && decoded.present?
          issues = judge(op.response, decoded.value)
          unless issues.empty?
            issue = issues.first
            path_text = Runtime.issue_path(issue.path)
            kept = mutation && mode == "strict" ? " The decoded body is in the result's partial; store any value shown only once from it before anything else." : ""
            redacted = Util.redact_paths(decoded.value, op.agent["sensitive_response_fields"])
            received = Util.get_path(redacted, issue.path)
            sensitive = issue.path.any? { |s| s.is_a?(String) && Util.looks_sensitive?(s) }
            problem = Runtime.diag(op.id, "UNEXPECTED_RESPONSE").status(status).request_id(rid).param("response#{path_text}")
                             .received(Util.envelope_value(received, sensitive)).expected(issue.message)
                             .remedy("The success response does not match the API description at response#{path_text} (#{issue.message}).#{after_effect}#{kept}")
                             .attempts(attempts).build
          end
        end
        if problem
          scrubbed = Util.scrub_diagnostic(problem, p.secrets)
          if mode == "strict"
            # The effect of a mutation happened: its body is never dropped,
            # since it can hold the only copy of a value.
            return Err.new(error: scrubbed, partial: decoded.value) if mutation && !decoded.invalid_json && decoded.present?

            return Err.new(error: scrubbed)
          end
          emit(scrubbed)
        end
        value = decoded.present? ? decoded.value : nil
        Ok.new(value: value, raw: value, meta: ResponseMeta.new(status: status, headers: headers, request_id: rid, attempts: attempts))
      end

      # A validator that only judges (responses, page items, events): a
      # failure is an issue at the root.
      def judge(validator, value)
        validator.validate(value)
      rescue StandardError
        [Issue.new(path: [], message: "the response schema failed (the validator raised an error)")]
      end

      # How to find out whether a mutation without replay protection took
      # effect after its answer was lost.
      def outcome_check(op, args)
        sent_args = Helpers.sent_arguments(op, args)
        hook_ = op.agent["verify"]
        if hook_ && hook_["operation"].to_s != "" && Classify.verify_callable?(hook_)
          scope = { "args" => Helpers.with_wire_names(op, args) }
          evaluated = Expr.evaluate(hook_["args"] || {}, scope)
          call = hook_["operation"] + Util.with_arguments(evaluated)
          lost = []
          expect = hook_["expect"].is_a?(Hash) ? resolve_lost(hook_["expect"], scope, lost) : {}
          fields = expect.is_a?(Hash) ? expect.keys : []
          shows =
            if fields.empty? then "whether it shows #{Classify.change_of(op)}"
            elsif !lost.empty?
              "whether #{fields.join(' and ')} holds what #{op.id} creates, matching the arguments you sent#{sent_args} (its #{lost.uniq.join(', ')} was in the lost response)"
            else "whether #{Expr.describe(expect)}"
            end
          return Classify::OutcomeCheck.new(call: call, shows: shows)
        end
        read = resource_read(op, args)
        return nil unless read

        Classify::OutcomeCheck.new(call: read[0] + Util.with_arguments(read[1]), shows: "whether it shows #{Classify.change_of(op)}")
      end

      def root_segment?(s) = s.empty? || s == "api" || s.match?(/\A[vV]\d+(\.\d+)*\z/)

      # A registered read of the resource op changes: a GET whose path is the
      # longest prefix of op's path (never the API root) whose required
      # parameters are path parameters op was called with.
      def resource_read(op, args)
        return nil if op.rpc

        known = {}
        op.params.each { |p| known[p.wire] = args[p.name] if p.location == "path" && !args[p.name].nil? }
        segments = op.path.split("/", -1)
        reads = @lock.synchronize { @registry.keys.sort.map { |id| @registry[id] } }
                     .select { |r| r.id != op.id && r.http_method == "GET" && !r.mutation? && r.rpc.nil? }
        segments.length.downto(1) do |n|
          prefix = segments.first(n)
          break if prefix.all? { |s| root_segment?(s) }

          path = prefix.join("/")
          reads.each do |read|
            next unless read.path == path

            params = read.argument_params
            next unless params.all? { |p| !p.required || (p.location == "path" && known.key?(p.wire)) }

            read_args = {}
            params.each { |p| read_args[p.wire] = known[p.wire] if p.location == "path" && known.key?(p.wire) }
            return [read.id, read_args]
          end
        end
        nil
      end

      def resolve_lost(node, scope, lost, depth = 0)
        return nil if depth > 32

        case node
        when String
          if node.start_with?("$response")
            rest = node.delete_prefix("$response").delete_prefix(".")
            lost << (rest.empty? ? "body" : rest)
            return "<#{node[1..]}>"
          end
          return node unless node.start_with?("$")

          v = Expr.resolve_ref(node, scope)
          UNSET.equal?(v) ? "<#{node[1..]}>" : v
        when Array then node.map { |n| resolve_lost(n, scope, lost, depth + 1) }
        when Hash then node.transform_values { |n| resolve_lost(n, scope, lost, depth + 1) }
        else node
        end
      end

      # ------------------------------------------------- previews and polls

      def preview_with(op, raw_args, opts)
        p = prepare(op, raw_args, opts, :preview, nil, nil)
        return p if p.is_a?(Err)

        conf = op.agent["confirmation"]
        effects = []
        effects << Helpers.interpolate(conf["message"], p.args, op) if conf && !conf["message"].to_s.empty?
        effects << op.agent["remediation_note"] unless op.agent["remediation_note"].to_s.empty?
        if conf && !(conf["summary_fields"] || []).empty?
          fields = conf["summary_fields"].map { |f| "#{f}={#{f}}" }
          effects << Helpers.interpolate("Arguments: #{fields.join(', ')}.", p.args, op)
        end
        effects << op.summary unless op.summary.empty?
        mutating = op.mutation?
        result = PreviewResult.new(
          operation: op.id, safety: op.agent["safety"],
          request: RenderedRequest.new(http_method: p.http_method, url: p.display_url, headers: p.headers.to_h(true), body: p.body.display),
          effects: effects, confirmation_token: mutating ? Keys.issue_token(@confirmation_key, op.id, p.args, now) : nil,
          expires_in_ms: mutating ? Keys::TTL_MS : nil, server_preview: UNSET, steps: nil
        )
        case op.agent["preview"]["mode"]
        when "header"
          dry = prepare(op, raw_args, opts, :server_preview, nil, nil)
          return dry if dry.is_a?(Err)

          answer = send_request(dry, opts)
          return answer if answer.is_a?(Err)

          shown = answer.raw.nil? ? UNSET : Util.redact_paths(answer.raw, op.agent["sensitive_response_fields"])
          Ok.new(value: result.with(server_preview: shown), meta: answer.meta)
        when "endpoint"
          name = op.agent["preview"]["operation"]
          target = operation(name)
          unless target
            return Runtime.diag(op.id, "VALIDATION_FAILED").param("operation").expected("the preview operation #{name} registered with the client")
                          .remedy("Register #{name} (ClientCore#register) so preview() can call it; nothing was sent.").err
          end
          answer = call_with(target, raw_args, opts.with(confirm: nil, verify: false), nil, nil)
          return answer if answer.is_a?(Err)

          fields = op.agent["sensitive_response_fields"] | target.agent["sensitive_response_fields"]
          shown = answer.raw.nil? ? UNSET : Util.redact_paths(answer.raw, fields)
          Ok.new(value: result.with(server_preview: shown), meta: answer.meta)
        else
          Ok.new(value: result, meta: ResponseMeta.local)
        end
      end

      def monotonic_ms = Transport.monotonic_ms

      # Poll until `until_` holds or the budget runs out: Ok whose value is
      # {"body" => last body (UNSET without one), "timed_out" => bool}.
      def poll_with(op, args, until_, interval_ms, budget_ms, opts, claim)
        started = monotonic_ms
        once = opts.with(verify: false)
        loop do
          response = call_with(op, args, once, nil, claim)
          return response if response.is_a?(Err)

          body = response.raw.nil? ? UNSET : response.raw
          done = Expr.predicate?(until_, body)
          out_of_time = !done && (monotonic_ms - started) + interval_ms > budget_ms
          if done || out_of_time
            return Ok.new(value: { "body" => body, "timed_out" => out_of_time }, raw: body, meta: response.meta)
          end

          sleep(interval_ms / 1000.0)
        end
      end

      def verify(op, args, body, opts)
        hook_ = op.agent["verify"]
        target = hook_ && operation(hook_["operation"])
        return Verification.unchecked unless target

        # The hook is written against the API: argument keys and $args
        # references use wire names.
        scope = { "args" => Helpers.with_wire_names(op, args) }
        scope["response"] = body unless UNSET.equal?(body)
        evaluated = Expr.evaluate(hook_["args"] || {}, scope)
        return Verification.unchecked unless evaluated.is_a?(Hash)

        mapped = evaluated.to_h { |k, v| [target.arg_name(k), v] }
        resolve = ->(predicate) { predicate.is_a?(Hash) && !predicate.empty? ? resolve_refs(predicate, scope) : nil }
        terminal = resolve.call(hook_["terminal"])
        expect = resolve.call(hook_["expect"]) || {}
        interval = hook_["poll_interval_ms"] || DEFAULT_POLL_INTERVAL_MS
        budget = hook_["poll_budget_ms"] || (terminal ? DEFAULT_VERIFY_BUDGET_MS : 0)
        rest = opts.with(confirm: nil, idempotency_key: nil, verify: false)
        polled = poll_with(target, mapped, terminal || expect, interval, budget, rest, nil)
        return Verification.unchecked(polled.error) if polled.is_a?(Err)

        last = polled.value["body"]
        Verification.new(checked: true, passed: Expr.predicate?(expect, last),
                         observed: UNSET.equal?(last) ? UNSET : Util.redact_paths(last, target.agent["sensitive_response_fields"]),
                         timed_out: budget != 0 && polled.value["timed_out"], error: nil)
      end

      UNRESOLVED = { "$tungsten_unresolved" => true }.freeze

      # References resolved; one that does not resolve equals nothing a
      # response can hold.
      def resolve_refs(node, scope, depth = 0)
        return UNRESOLVED.dup if depth > 64

        case node
        when String
          return node unless node.start_with?("$")

          v = Expr.resolve_ref(node, scope)
          UNSET.equal?(v) || v.nil? ? UNRESOLVED.dup : v
        when Array then node.map { |n| resolve_refs(n, scope, depth + 1) }
        when Hash then node.transform_values { |n| resolve_refs(n, scope, depth + 1) }
        else node
        end
      end
    end
  end
end
