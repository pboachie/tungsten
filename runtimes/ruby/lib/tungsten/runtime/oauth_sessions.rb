# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # One client's bounded managed OAuth state. Store predicates take this lock;
    # no store, descriptor, clock hook or other application code runs under it.
    class OAuthSessions
      BEGIN_REFUSAL = "Cannot begin authorization: supply bounded valid configuration, S256 and issuer qualification, an owning session binding and an atomic fenced token store."
      CALLBACK_REFUSAL = "Authorization callback refused: invalid, expired, consumed or mismatched attempt. Begin a new managed authorization attempt if needed."
      SESSION_REFUSAL = "The managed OAuth session changed or its work deadline expired; begin a new managed authorization attempt if needed."
      MISSING = "No OAuth2 token is stored; begin authorization_url(), then complete exchange_code() with its handle, full callback URL and owning session binding."
      LOGOUT_REFUSAL = "Local OAuth work was fenced, but credential removal did not complete. Check the token store and retry logout before using stored credentials."

      def initialize(core)
        @core = core
        @lock = Mutex.new
        @condition = ConditionVariable.new
        @sessions = {}
        @attempts = {}
        @flights = {}
        @owners = 0
        @admitted = 0
        @expiry = nil
        # Scheme state cannot grow through arbitrary helper names or descriptor
        # mutation. Bound and capture the initial known code-flow names once.
        budget = OAuthInputs::Budget.new
        core.api.fetch("auth", []).each do |scheme|
          next unless scheme.is_a?(Hash) && scheme["kind"] == "oauth2" && scheme["authorization_code"]
          name = budget.text(scheme["name"])
          OAuthInputs.require_valid(!name.empty? && @sessions.length < OAuthInputs::CAPACITY && !@sessions.key?(name))
          @sessions[name] = { generation: 0, epoch: 0, logging_out: false, selected: true }
        end
      rescue StandardError
        @sessions.clear
      end

      def inspect = "#<Tungsten::Runtime::OAuthSessions #{REDACTED}>"
      alias_method :to_s, :inspect

      def capable?
        @core.token_store.respond_to?(:set_if_current) && @core.token_store.respond_to?(:delete_if_current)
      end

      def version_locked(scheme)
        state = @sessions[scheme]
        state && [state[:generation], state[:epoch]].freeze
      end

      def current_locked?(scheme, version, selected: false)
        state = @sessions[scheme]
        state && !state[:logging_out] && version == [state[:generation], state[:epoch]] && (!selected || state[:selected])
      end

      def current?(scheme, version, selected: false)
        @lock.synchronize { current_locked?(scheme, version, selected: selected) }
      end

      def clock = Transport.monotonic_ms

      def timely?(job) = clock < job[:deadline]

      def owner(scheme)
        duration = @core.timeout_ms
        OAuthInputs.require_valid(OAuthInputs.safe_integer?(duration))
        started = clock
        @lock.synchronize do
          version = version_locked(scheme)
          OAuthInputs.require_valid(version && current_locked?(scheme, version) && @owners < OAuthInputs::CAPACITY)
          @owners += 1
          { version: version, deadline: started + duration }
        end
      end

      def release_owner
        @lock.synchronize { @owners -= 1 }
      end

      def failure = ClientCore::TokenFailure.new(SESSION_REFUSAL, false)

      def error(scheme, helper, message)
        # Never interpolate caller, provider or custom exception strings.
        @core.send(:oauth_error, "oauth.#{scheme}.#{helper}", message)
      end

      def operation_scheme(scheme)
        copied = OAuthInputs.text(scheme, OAuthInputs::INPUT_BYTES)
        known = @lock.synchronize { @sessions.key?(copied) }
        known && copied.match?(/\A[a-zA-Z0-9_.-]{1,64}\z/) ? copied : "managed"
      rescue StandardError
        "managed"
      end

      def raw_part(parts, *names)
        key = names.find { |name| parts.key?(name) }
        key ? parts[key] : nil
      end

      def configuration(scheme, budget)
        budget.text(scheme)
        flow, = @core.send(:code_flow, scheme)
        OAuthInputs.require_valid(flow.is_a?(Hash))
        parts = @core.instance_variable_get(:@auth)[scheme]
        OAuthInputs.require_valid(parts.is_a?(Hash) && raw_part(parts, "flow", :flow) == "authorizationCode")
        id = budget.text(raw_part(parts, "client_id", :client_id, "clientId", :clientId))
        OAuthInputs.require_valid(!id.empty?)
        client = {
          id: id, secret: budget.text(raw_part(parts, "client_secret", :client_secret, "clientSecret", :clientSecret) || ""),
          redirect: budget.text(raw_part(parts, "redirect_uri", :redirect_uri, "redirectUri", :redirectUri) || "")
        }.freeze
        result = {
          client: client, authorization_url: budget.text(flow["authorization_url"]),
          token_url: budget.text(flow["token_url"]), refresh_url: budget.optional(flow["refresh_url"]),
          configured_scopes: budget.scopes(flow["scopes"] || [])
        }
        OAuthInputs.endpoint(result[:authorization_url])
        OAuthInputs.endpoint(result[:token_url])
        OAuthInputs.endpoint(result[:refresh_url]) if result[:refresh_url] && !result[:refresh_url].empty?
        result.freeze
      end

      def configuration_current?(scheme, capture)
        fresh = configuration(scheme, OAuthInputs::Budget.new)
        fresh == capture[:configuration]
      rescue StandardError
        false
      end

      def capture(scheme, parameters)
        budget = OAuthInputs::Budget.new
        config = configuration(scheme, budget)
        binding = budget.text(parameters[:session_binding])
        OAuthInputs.require_valid(!binding.empty? && parameters[:s256_supported] == true)
        defense = parameters[:issuer_defense]
        OAuthInputs.require_valid(defense.is_a?(IssuerDefense) && %i[response distinct].include?(defense.mode))
        issuer = budget.text(defense.value)
        OAuthInputs.require_valid(!issuer.empty?)
        OAuthInputs.endpoint(issuer) if defense.mode == :response
        redirect = budget.text(parameters[:redirect_uri] || config[:client][:redirect])
        OAuthInputs.endpoint(redirect, redirect: true)
        scopes = parameters[:scopes].nil? ? config[:configured_scopes] : budget.scopes(parameters[:scopes])
        extra = budget.extra(parameters[:extra] || {})
        resource, audience, context = %i[resource audience account_context].map { |key| budget.optional(parameters[key]) }
        state = Util.base64url(SecureRandom.random_bytes(32)).freeze
        verifier = Util.base64url(SecureRandom.random_bytes(32)).freeze
        fields = [["response_type", "code"], ["client_id", config[:client][:id]], ["redirect_uri", redirect],
                  ["state", state], ["code_challenge", Pkce.challenge_of(verifier)], ["code_challenge_method", "S256"]]
        fields << ["scope", scopes.join(" ")] unless scopes.empty?
        fields << ["resource", resource] if resource
        fields << ["audience", audience] if audience
        fields.concat(extra.to_a)
        base = config[:authorization_url]
        separator = base.include?("?") ? (base.end_with?("?", "&") ? "" : "&") : "?"
        {
          configuration: config, binding: binding, issuer_mode: defense.mode, issuer: issuer,
          redirect: redirect, scopes: scopes, resource: resource, audience: audience, account_context: context,
          state: state, verifier: verifier, url: (base + separator + @core.send(:form_body, fields)).freeze
        }.freeze
      end

      def live_locked?(attempt, at)
        state = @sessions[attempt[:scheme]]
        !attempt[:invalid] && at < attempt[:expires] &&
          state && !state[:logging_out] && state[:generation] == attempt[:generation]
      end

      def remove_locked(handle)
        if @attempts.delete(handle)
          @admitted -= 1
          @condition.broadcast
        end
      end

      def invalidate_locked(handle, attempt)
        attempt[:invalid] = true
        attempt[:capture] = nil
        attempt[:binding] = nil
        remove_locked(handle) unless attempt[:claimed]
      end

      def sweep_locked(at)
        @attempts.to_a.each do |handle, attempt|
          invalidate_locked(handle, attempt) unless live_locked?(attempt, at)
        end
      end

      def start_expiry_locked
        return if @expiry
        @expiry = Thread.new do
          @lock.synchronize do
            loop do
              at = clock
              sweep_locked(at)
              deadlines = @attempts.values.filter_map { |attempt| attempt[:expires] unless attempt[:invalid] }
              break if deadlines.empty?
              @condition.wait(@lock, [(deadlines.min - at) / 1000.0, 0.001].max)
            end
            @expiry = nil
          end
        rescue StandardError
          @lock.synchronize do
            @attempts.to_a.each { |handle, attempt| invalidate_locked(handle, attempt) }
            @expiry = nil
            @condition.broadcast
          end
        end
        @expiry.report_on_exception = false
      end

      def begin_authorization(scheme, parameters)
        reserved = false
        handle = nil
        published = false
        generation = nil
        scheme = OAuthInputs.text(scheme, OAuthInputs::INPUT_BYTES)
        safe_scheme = operation_scheme(scheme)
        at = clock
        @lock.synchronize do
          sweep_locked(at)
          state = @sessions[scheme]
          OAuthInputs.require_valid(state && !state[:logging_out] && @admitted < OAuthInputs::CAPACITY)
          @admitted += 1
          reserved = true
          generation = state[:generation]
        end
        OAuthInputs.require_valid(capable? && OAuthInputs.safe_integer?(@core.max_oauth_token_response_bytes))
        captured = capture(scheme, parameters)
        # No provider work is allowed while a begin is still capturing inputs.
        OAuthInputs.require_valid(configuration_current?(scheme, captured))
        candidate = Util.base64url(SecureRandom.random_bytes(32)).freeze
        at = clock
        @lock.synchronize do
          state = @sessions[scheme]
          OAuthInputs.require_valid(!state[:logging_out] && state[:generation] == generation)
          OAuthInputs.require_valid(!@attempts.key?(candidate))
          handle = candidate
          @attempts[handle] = { scheme: scheme, generation: state[:generation], expires: at + OAuthInputs::LIFETIME_MS,
                                capture: captured, binding: captured[:binding], invalid: false, claimed: false }
          reserved = false
          start_expiry_locked
          @condition.broadcast
          published = true
        end
        Ok.new(value: AuthorizationAttempt.new(attempt_handle: handle, url: captured[:url], expires_in_ms: OAuthInputs::LIFETIME_MS),
               meta: ResponseMeta.local)
      rescue StandardError
        error(safe_scheme || "managed", "authorizationUrl", BEGIN_REFUSAL)
      ensure
        @lock.synchronize do
          remove_locked(handle) if handle && !published
          @admitted -= 1 if reserved
        end
      end

      def cancel(scheme, handle, binding)
        scheme = OAuthInputs.text(scheme, OAuthInputs::INPUT_BYTES)
        handle = OAuthInputs.text(handle, OAuthInputs::INPUT_BYTES)
        binding = OAuthInputs.text(binding, OAuthInputs::INPUT_BYTES)
        @lock.synchronize do
          attempt = @attempts[handle]
          OAuthInputs.require_valid(attempt && attempt[:scheme] == scheme && !attempt[:invalid] && attempt[:binding] == binding)
          invalidate_locked(handle, attempt)
          @condition.broadcast
        end
        Ok.new(value: nil, meta: ResponseMeta.local)
      rescue StandardError
        error(operation_scheme(scheme), "cancelAuthorization", CALLBACK_REFUSAL)
      end

      def complete(scheme, handle, callback_url, binding)
        scheme = OAuthInputs.text(scheme, OAuthInputs::INPUT_BYTES)
        safe_scheme = operation_scheme(scheme)
        handle = OAuthInputs.text(handle, OAuthInputs::INPUT_BYTES)
        binding = OAuthInputs.text(binding, OAuthInputs::INPUT_BYTES)
        captured = nil
        claimed = false
        job = nil
        at = clock
        @lock.synchronize do
          sweep_locked(at)
          attempt = @attempts[handle]
          OAuthInputs.require_valid(attempt && attempt[:scheme] == scheme && !attempt[:claimed] && live_locked?(attempt, at))
          captured = attempt[:capture]
          OAuthInputs.require_valid(captured[:binding] == binding)
        end
        kind, code = OAuthInputs.callback(callback_url, captured)
        OAuthInputs.require_valid(configuration_current?(scheme, captured))
        job = owner(scheme)
        at = clock
        @lock.synchronize do
          attempt = @attempts[handle]
          OAuthInputs.require_valid(attempt && !attempt[:claimed] && live_locked?(attempt, at) && attempt[:capture].equal?(captured))
          attempt[:claimed] = true
          attempt[:capture] = nil # The physical owner's local immutable capture retains it until settlement.
          claimed = true
        end
        return error(safe_scheme, "exchangeCode", CALLBACK_REFUSAL) if kind == :denied
        OAuthInputs.require_valid(timely?(job) && configuration_current?(scheme, captured))
        at = clock
        # Dispatch commits at this gate after the last application descriptor
        # access. A cancel/logout winning first sends nothing; after this gate
        # a request may be sent, but admission and persistence fences remain.
        @lock.synchronize do
          attempt = @attempts[handle]
          OAuthInputs.require_valid(at < job[:deadline] && attempt && !attempt[:invalid] && at < attempt[:expires] &&
                                    attempt[:generation] == @sessions[scheme][:generation] && !@sessions[scheme][:logging_out])
        end
        fields = [["grant_type", "authorization_code"], ["code", code], ["redirect_uri", captured[:redirect]], ["code_verifier", captured[:verifier]]]
        stored, problem = @core.send(:token_request, scheme, captured[:configuration][:token_url], captured[:configuration][:client], fields, "",
                                    deadline: job[:deadline])
        return error(safe_scheme, "exchangeCode", problem.message) if problem
        committed = nil
        changed = @core.token_store.set_if_current(scheme, stored) do |_previous|
          configured = configuration_current?(scheme, captured)
          at = clock
          @lock.synchronize do
            attempt = @attempts[handle]
            state = @sessions[scheme]
            if configured && at < job[:deadline] && attempt && !attempt[:invalid] && at < attempt[:expires] &&
               state[:generation] == attempt[:generation] && !state[:logging_out]
              state[:epoch] += 1
              committed = version_locked(scheme)
              true
            else
              false
            end
          end
        end
        configured = configuration_current?(scheme, captured)
        at = clock
        @lock.synchronize do
          attempt = @attempts[handle]
          OAuthInputs.require_valid(changed == true && committed && configured && at < job[:deadline] &&
                                    attempt && !attempt[:invalid] && at < attempt[:expires] && current_locked?(scheme, committed))
          @sessions[scheme][:selected] = true
          remove_locked(handle) # Success publication and terminal retirement linearize together.
          claimed = false # This owner has retired; ensure must not remove a reused handle.
        end
        Ok.new(value: @core.send(:token_info, scheme, stored), meta: ResponseMeta.local)
      rescue StandardError
        error(safe_scheme || "managed", "exchangeCode", CALLBACK_REFUSAL)
      ensure
        @lock.synchronize { remove_locked(handle) } if claimed
        release_owner if job
      end

      def logout(scheme)
        version = nil
        job = nil
        scheme = OAuthInputs.text(scheme, OAuthInputs::INPUT_BYTES)
        safe_scheme = operation_scheme(scheme)
        # Fence first, even if cleanup capacity/configuration/capability fails.
        @lock.synchronize do
          state = @sessions[scheme]
          OAuthInputs.require_valid(state)
          state[:generation] += 1
          state[:selected] = false
          state[:logging_out] = true
          version = version_locked(scheme)
          @attempts.to_a.each { |handle, attempt| invalidate_locked(handle, attempt) if attempt[:scheme] == scheme }
          @condition.broadcast
        end
        duration = @core.timeout_ms
        OAuthInputs.require_valid(capable? && OAuthInputs.safe_integer?(duration))
        started = clock
        @lock.synchronize do
          OAuthInputs.require_valid(@owners < OAuthInputs::CAPACITY)
          @owners += 1
          job = { deadline: started + duration }
        end
        changed = @core.token_store.delete_if_current(scheme) do |_previous|
          at = clock
          @lock.synchronize do
            state = @sessions[scheme]
            allowed = at < job[:deadline] && version == version_locked(scheme) && state[:logging_out]
            if allowed
              state[:epoch] += 1
              version = version_locked(scheme)
            end
            allowed
          end
        end
        OAuthInputs.require_valid(changed == true && timely?(job))
        Ok.new(value: nil, meta: ResponseMeta.local)
      rescue StandardError
        error(safe_scheme || "managed", "logout", LOGOUT_REFUSAL)
      ensure
        if version
          @lock.synchronize do
            state = @sessions[scheme]
            state[:logging_out] = false if state && state[:generation] == version[0]
          end
        end
        release_owner if job
      end

      def snapshot_token(stored)
        return nil unless stored.is_a?(StoredToken)
        limit = @core.max_oauth_token_response_bytes
        access = OAuthInputs.text(stored.access_token, limit)
        return nil if access.empty?
        refresh = stored.refresh_token.nil? ? nil : OAuthInputs.text(stored.refresh_token, limit)
        expires = stored.expires_at
        OAuthInputs.require_valid(expires.nil? || expires.instance_of?(Integer))
        kind = stored.token_type.nil? ? nil : OAuthInputs.text(stored.token_type, limit)
        scope = stored.scope.nil? ? nil : OAuthInputs.text(stored.scope, limit)
        StoredToken.new(access_token: access, refresh_token: refresh, expires_at: expires,
                        token_type: kind, scope: scope)
      end

      def same_token?(left, right)
        left.is_a?(StoredToken) && left.access_token == right.access_token && left.refresh_token == right.refresh_token
      end

      # Returns the stored record as well as the access token, so explicit
      # refresh never performs a second unowned custom-store read.
      def token(scheme, force, rejected: nil)
        job = nil
        flight = nil
        leader = false
        key = nil
        scheme = OAuthInputs.text(scheme, OAuthInputs::INPUT_BYTES)
        job = owner(scheme) # Charge before custom configuration, Get and Now.
        OAuthInputs.require_valid(capable? && OAuthInputs.safe_integer?(@core.max_oauth_token_response_bytes))
        config = configuration(scheme, OAuthInputs::Budget.new)
        capture = { configuration: config }.freeze
        OAuthInputs.require_valid(timely?(job) && current?(scheme, job[:version], selected: true))
        stored = snapshot_token(@core.token_store.get(scheme))
        OAuthInputs.require_valid(timely?(job) && current?(scheme, job[:version], selected: true))
        return [nil, ClientCore::TokenFailure.new(MISSING, false)] unless stored
        fresh = stored.expires_at.nil? || stored.expires_at > @core.send(:now) + EXPIRY_SKEW_MS
        OAuthInputs.require_valid(timely?(job) && current?(scheme, job[:version], selected: true))
        return [stored, nil] if (!force && fresh) || (rejected && stored.access_token != rejected)
        OAuthInputs.require_valid(stored.refresh_token && !stored.refresh_token.empty?)
        @lock.synchronize do
          OAuthInputs.require_valid(current_locked?(scheme, job[:version], selected: true))
          key = [scheme, *job[:version]].freeze
          flight = @flights[key]
          unless flight
            flight = { done: false, result: nil }
            @flights[key] = flight
            leader = true
          end
          unless leader
            until flight[:done]
              remaining = job[:deadline] - clock
              OAuthInputs.require_valid(remaining.positive?)
              @condition.wait(@lock, remaining / 1000.0)
            end
          end
        end
        unless leader
          result = flight[:result]
          OAuthInputs.require_valid(timely?(job) && current?(scheme, result[:version], selected: true))
          return [result[:stored], result[:failure]]
        end
        result = refresh(scheme, capture, stored, job)
        in_time = timely?(job)
        @lock.synchronize do
          OAuthInputs.require_valid(!result[:stored] || (in_time && current_locked?(scheme, result[:version], selected: true)))
          flight[:result] = result
          flight[:done] = true
          @condition.broadcast
        end
        [result[:stored], result[:failure]]
      rescue StandardError
        [nil, failure]
      ensure
        if leader
          @lock.synchronize do
            unless flight[:done]
              flight[:result] = { stored: nil, failure: failure, version: job[:version] }.freeze
              flight[:done] = true
            end
            @flights.delete(key) if @flights[key].equal?(flight)
            @condition.broadcast
          end
        end
        release_owner if job
      end

      def refresh(scheme, capture, stored, job)
        config = capture[:configuration]
        url = config[:refresh_url]
        url = config[:token_url] if url.nil? || url.empty?
        OAuthInputs.endpoint(url) # Validate the actual selected endpoint.
        OAuthInputs.require_valid(timely?(job) && configuration_current?(scheme, capture))
        at = clock
        @lock.synchronize do
          OAuthInputs.require_valid(at < job[:deadline] && current_locked?(scheme, job[:version], selected: true))
        end
        renewed, problem = @core.send(:token_request, scheme, url, config[:client],
                                     [["grant_type", "refresh_token"], ["refresh_token", stored.refresh_token]], stored.refresh_token,
                                     deadline: job[:deadline])
        committed = nil
        predicate = lambda do |previous|
          configured = configuration_current?(scheme, capture)
          matches = same_token?(previous, stored)
          at = clock
          @lock.synchronize do
            allowed = configured && at < job[:deadline] && current_locked?(scheme, job[:version], selected: true) &&
                      matches
            if allowed
              @sessions[scheme][:epoch] += 1
              committed = version_locked(scheme)
            end
            allowed
          end
        end
        if problem
          @core.token_store.delete_if_current(scheme, &predicate) if problem.rejected
          return { stored: nil, failure: problem, version: committed || job[:version] }.freeze
        end
        changed = @core.token_store.set_if_current(scheme, renewed, &predicate)
        OAuthInputs.require_valid(changed == true && committed && timely?(job) &&
                                  configuration_current?(scheme, capture) && timely?(job) && current?(scheme, committed, selected: true))
        { stored: renewed, failure: nil, version: committed }.freeze
      end
    end
  end
end
