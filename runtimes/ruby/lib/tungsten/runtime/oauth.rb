# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # Tokens expiring within this many milliseconds count as expired.
    EXPIRY_SKEW_MS = 30_000

    # What a token store keeps for one scheme. `inspect` shows no secret.
    StoredToken = Data.define(:access_token, :refresh_token, :expires_at, :token_type, :scope) do
      def initialize(access_token:, refresh_token: nil, expires_at: nil, token_type: nil, scope: nil) = super
      def inspect = "#<Tungsten::Runtime::StoredToken #{REDACTED}>"
      alias_method :to_s, :inspect
    end

    # The default token store of the authorization-code schemes: in memory,
    # per client. Any object with `get(scheme)`, `set(scheme, token)` and
    # `delete(scheme)` can replace it (ClientOptions `token_store`).
    class MemoryTokenStore
      def initialize
        @tokens = {}
        @lock = Mutex.new
      end

      def get(scheme) = @lock.synchronize { @tokens[scheme] }
      def set(scheme, token) = @lock.synchronize { @tokens[scheme] = token }
      def delete(scheme) = @lock.synchronize { @tokens.delete(scheme) }
    end

    # A PKCE pair (RFC 7636, S256); the verifier is a secret until the
    # exchange.
    Pkce = Data.define(:verifier, :challenge) do
      def inspect = "#<Tungsten::Runtime::Pkce challenge=#{challenge}>"
      alias_method :to_s, :inspect

      # base64url(SHA-256(ASCII(verifier))).
      def self.challenge_of(verifier) = Util.base64url(Digest::SHA256.digest(verifier))

      # A fresh pair: a 43 character verifier from 32 random bytes.
      def self.generate
        verifier = Util.base64url(SecureRandom.random_bytes(32))
        new(verifier: verifier, challenge: challenge_of(verifier))
      end
    end

    # What exchange_code and refresh report: no secrets.
    TokenInfo = Data.define(:scheme, :expires_at, :scope, :refreshable)

    # The authorization-code helpers of one OAuth2 scheme (`client.oauth(name)`).
    # Failures are AUTH_FAILED envelopes in Err.
    class OAuthFlow
      attr_reader :scheme

      def initialize(core, scheme)
        @core = core
        @scheme = scheme
      end

      def token_store = @core.token_store

      # A fresh PKCE pair: pass `challenge` to authorization_url and keep
      # `verifier` for exchange_code.
      def pkce = Pkce.generate

      # The URL to send the user to: Ok with the URL, or Err.
      def authorization_url(redirect_uri: nil, scopes: nil, state: nil, code_challenge: nil, extra: {})
        @core.send(:authorization_url, @scheme, redirect_uri: redirect_uri, scopes: scopes, state: state,
                                                code_challenge: code_challenge, extra: extra)
      end

      # Trade the code from the redirect for tokens, stored for later calls.
      def exchange_code(code:, redirect_uri: nil, code_verifier: nil)
        @core.send(:exchange_code, @scheme, code: code, redirect_uri: redirect_uri, code_verifier: code_verifier)
      end

      # Refresh the stored token now (calls refresh on their own when it
      # expires).
      def refresh = @core.send(:refresh_now, @scheme)
    end

    class ClientCore
      # The authorization-code helpers of a scheme.
      def oauth(scheme) = OAuthFlow.new(self, scheme)

      # The store of authorization-code tokens.
      attr_reader :token_store

      private

      TokenFailure = Struct.new(:message, :rejected)

      def form_body(fields) = fields.map { |k, v| "#{Serialize.percent_encode(k)}=#{Serialize.percent_encode(v)}" }.join("&")

      def code_flow(scheme)
        s = schemes.find { |x| x["name"] == scheme }
        return [nil, "the API descriptor defines no scheme named #{scheme}"] unless s
        return [s["authorization_code"], nil] if s["kind"] == "oauth2" && s["authorization_code"]

        [nil, "the scheme #{scheme} has no authorizationCode flow"]
      end

      def code_client(scheme)
        invalid = "auth.#{scheme} must be a Hash with flow \"authorizationCode\", client_id, and optionally client_secret and redirect_uri"
        cred = credential(scheme)
        return [nil, invalid] unless cred.is_a?(Hash) && part_of(cred, "flow", "flow") == "authorizationCode"

        id = part_of(cred, "client_id", "clientId").to_s
        return [nil, "auth.#{scheme} needs a client_id"] if id.empty?

        [{ id: id, secret: part_of(cred, "client_secret", "clientSecret").to_s,
           redirect: part_of(cred, "redirect_uri", "redirectUri").to_s }, nil]
      end

      def oauth_error(operation, remediation) = Runtime.diag(operation, "AUTH_FAILED").remedy(remediation).err

      def read_token(scheme)
        t = @token_store.get(scheme)
        t.is_a?(StoredToken) && t.access_token.is_a?(String) && !t.access_token.empty? ? t : nil
      end

      def gate_of(scheme) = @lock.synchronize { @oauth_gates[scheme] ||= { mutex: Mutex.new, generation: 0 } }

      # [token, nil] or [nil, remediation].
      def stored_token(scheme, force)
        token, failure = stored_token_inner(scheme, force)
        failure ? [nil, failure.message] : [token, nil]
      end

      def stored_token_inner(scheme, force)
        missing = TokenFailure.new(
          "No OAuth2 token is stored for #{scheme}; send the user to authorization_url(), then call exchange_code() " \
          "with the code, or put a token in ClientOptions.token_store.", false
        )
        stored = read_token(scheme)
        return [nil, missing] unless stored

        fresh = stored.expires_at.nil? || stored.expires_at > now + EXPIRY_SKEW_MS
        return [stored.access_token, nil] if fresh && !force

        gate = gate_of(scheme)
        seen = @lock.synchronize { gate[:generation] }
        gate[:mutex].synchronize do
          if @lock.synchronize { gate[:generation] } != seen
            t = read_token(scheme)
            return t ? [t.access_token, nil] : [nil, missing]
          end
          current = read_token(scheme)
          return [nil, missing] unless current

          token, failure = refresh_stored(scheme, current, force)
          return [nil, failure] if failure

          @lock.synchronize { gate[:generation] += 1 }
          [token, nil]
        end
      end

      # The refreshed token after a 401 when the stored token has a refresh
      # token, else nil.
      def refresh_after_rejection(scheme, rejected)
        stored = read_token(scheme)
        return nil unless stored
        return stored.access_token if stored.access_token != rejected
        return nil if stored.refresh_token.to_s.empty?

        token, problem = stored_token(scheme, true)
        problem ? nil : token
      end

      def refresh_stored(scheme, stored, force)
        flow, reason = code_flow(scheme)
        return [nil, TokenFailure.new("Cannot refresh the OAuth2 token for #{scheme}: #{reason}.", false)] unless flow

        client, reason = code_client(scheme)
        return [nil, TokenFailure.new("Cannot refresh the OAuth2 token for #{scheme}: #{reason}.", false)] unless client

        if stored.refresh_token.to_s.empty?
          why = "#{force ? '' : 'expired and '}has no refresh token"
          return [nil, TokenFailure.new("The OAuth2 token for #{scheme} #{why}; authorize again with authorization_url() and exchange_code().", false)]
        end
        url = flow["refresh_url"] || flow["token_url"]
        renewed, failure = token_request(scheme, url, client, [["grant_type", "refresh_token"], ["refresh_token", stored.refresh_token]], stored.refresh_token)
        if failure
          @token_store.delete(scheme) if failure.rejected
          return [nil, failure]
        end
        @token_store.set(scheme, renewed)
        [renewed.access_token, nil]
      end

      def token_timeout_ms = [@timeout_ms, 1].max

      def token_request(scheme, url, client, fields, previous)
        return [nil, TokenFailure.new(INSECURE_HTTP_AUTH, false)] if insecure_http?(url)
        headers = [["Content-Type", "application/x-www-form-urlencoded"], ["Accept", "application/json"]]
        all = fields.dup
        if client[:secret].empty?
          all << ["client_id", client[:id]]
        else
          basic = Base64.strict_encode64("#{Serialize.percent_encode(client[:id])}:#{Serialize.percent_encode(client[:secret])}")
          headers << ["Authorization", "Basic #{basic}"]
        end
        body = Serialize::Encoded.new(empty: false, bytes: form_body(all).b, parts: nil, content_type: nil, display: nil, hashed: nil)
        out = Transport.attempt(url: url.to_s, method: "POST", headers: headers, body: body, timeout_ms: token_timeout_ms)
        unless out.kind == :response && out.body
          return [nil, TokenFailure.new("The OAuth2 token endpoint for #{scheme} could not be reached; check network access and the token URL.", false)]
        end

        decoded = Classify.decode_body(out.body, out.headers, "application/json")
        unless out.status.between?(200, 299)
          code = Util.get_path_str(decoded.value, "error")
          named = code.is_a?(String) && code.match?(/\A[a-z_]{1,40}\z/) ? " (#{code})" : ""
          return [nil, TokenFailure.new(
            "The OAuth2 token endpoint for #{scheme} answered HTTP #{out.status}#{named}; check the code or refresh token, client credentials and redirect URI.",
            out.status.between?(400, 499)
          )]
        end
        text = ->(path) { Util.get_path_str(decoded.value, path).then { |v| v.is_a?(String) ? v : "" } }
        access = text.call("access_token")
        return [nil, TokenFailure.new("The OAuth2 token endpoint for #{scheme} returned no access_token.", false)] if access.empty?

        refresh = text.call("refresh_token")
        refresh = previous if refresh.empty?
        expires_in = Util.get_path_str(decoded.value, "expires_in")
        expires = expires_in.is_a?(Numeric) && expires_in.finite? ? now + ([expires_in, 0].max * 1000).to_i : nil
        token = StoredToken.new(access_token: access, refresh_token: refresh.empty? ? nil : refresh, expires_at: expires,
                                token_type: text.call("token_type"), scope: text.call("scope"))
        [token, nil]
      end

      # An OAuth2 client-credentials token, fetched once per scheme and
      # cached until 30 s before it expires (one request at a time).
      def client_credentials_token(sat)
        return [nil, INSECURE_HTTP_AUTH] if insecure_http?(sat.token_url)
        @cc_lock.synchronize do
          real_now = (Time.now.to_f * 1000).to_i
          cached = @cc_tokens[sat.name]
          return [cached[:token], nil] if cached && cached[:expires_at] > real_now + 30_000

          form = ["grant_type=client_credentials"]
          form << "scope=#{Serialize.encode_form_component(sat.scopes.join(' '))}" unless sat.scopes.empty?
          basic = Base64.strict_encode64("#{Serialize.encode_component(sat.client_id)}:#{Serialize.encode_component(sat.client_secret)}")
          headers = [["Content-Type", "application/x-www-form-urlencoded"], ["Accept", "application/json"], ["Authorization", "Basic #{basic}"]]
          body = Serialize::Encoded.new(empty: false, bytes: form.join("&").b, parts: nil, content_type: nil, display: nil, hashed: nil)
          out = Transport.attempt(url: sat.token_url, method: "POST", headers: headers, body: body, timeout_ms: token_timeout_ms)
          unless out.kind == :response && out.body
            return [nil, "The OAuth2 token endpoint for #{sat.name} could not be reached; check network access and the token URL."]
          end
          unless out.status.between?(200, 299)
            return [nil, "The OAuth2 token endpoint for #{sat.name} answered HTTP #{out.status}; check client_id, client_secret and scopes."]
          end

          decoded = Classify.decode_body(out.body, out.headers, "application/json")
          token = Util.get_path_str(decoded.value, "access_token")
          return [nil, "The OAuth2 token endpoint for #{sat.name} returned no access_token."] unless token.is_a?(String) && !token.empty?

          lifetime = 3_600_000
          e = Util.get_path_str(decoded.value, "expires_in")
          lifetime = [(e * 1000).to_i, 0].max if e.is_a?(Numeric) && e.finite?
          @cc_tokens[sat.name] = { token: token, expires_at: real_now + lifetime }
          [token, nil]
        end
      end

      def oauth_operation(scheme, helper) = "oauth.#{scheme}.#{helper}"

      def authorization_url(scheme, redirect_uri:, scopes:, state:, code_challenge:, extra:)
        operation = oauth_operation(scheme, "authorizationUrl")
        flow, reason = code_flow(scheme)
        return oauth_error(operation, "Cannot build an authorization URL: #{reason}.") unless flow

        client, reason = code_client(scheme)
        return oauth_error(operation, "Cannot build an authorization URL: #{reason}.") unless client

        redirect = redirect_uri.to_s.empty? ? client[:redirect] : redirect_uri.to_s
        if redirect.empty?
          return oauth_error(operation, "Pass redirect_uri, or set redirect_uri in auth.#{scheme}; the authorization URL needs one.")
        end

        scope = (scopes.nil? ? flow["scopes"] || [] : scopes).join(" ")
        fields = [["response_type", "code"], ["client_id", client[:id]], ["redirect_uri", redirect]]
        fields << ["scope", scope] unless scope.empty?
        fields << ["state", state] unless state.to_s.empty?
        fields.push(["code_challenge", code_challenge], %w[code_challenge_method S256]) unless code_challenge.to_s.empty?
        reserved = fields.map(&:first)
        (extra || {}).transform_keys(&:to_s).sort.each { |k, v| fields << [k, v.to_s] unless reserved.include?(k) }
        base = flow["authorization_url"].to_s
        sep = base.include?("?") ? (base.end_with?("?", "&") ? "" : "&") : "?"
        Ok.new(value: base + sep + form_body(fields), meta: ResponseMeta.local)
      end

      def exchange_code(scheme, code:, redirect_uri:, code_verifier:)
        operation = oauth_operation(scheme, "exchangeCode")
        flow, reason = code_flow(scheme)
        return oauth_error(operation, "Cannot exchange the code: #{reason}.") unless flow

        client, reason = code_client(scheme)
        return oauth_error(operation, "Cannot exchange the code: #{reason}.") unless client
        return oauth_error(operation, "Pass the authorization code the server redirected back with as code.") if code.to_s.empty?

        redirect = redirect_uri.to_s.empty? ? client[:redirect] : redirect_uri.to_s
        if redirect.empty?
          return oauth_error(operation, "Pass redirect_uri, or set redirect_uri in auth.#{scheme}; it must equal the one used for the authorization URL.")
        end

        fields = [["grant_type", "authorization_code"], ["code", code.to_s], ["redirect_uri", redirect]]
        fields << ["code_verifier", code_verifier.to_s] unless code_verifier.to_s.empty?
        stored, failure = token_request(scheme, flow["token_url"], client, fields, "")
        return oauth_error(operation, failure.message) if failure

        @token_store.set(scheme, stored)
        Ok.new(value: token_info(scheme, stored), meta: ResponseMeta.local)
      end

      def refresh_now(scheme)
        operation = oauth_operation(scheme, "refresh")
        _, failure = stored_token_inner(scheme, true)
        return oauth_error(operation, failure.message) if failure

        stored = read_token(scheme)
        return oauth_error(operation, "No token is stored for #{scheme}.") unless stored

        Ok.new(value: token_info(scheme, stored), meta: ResponseMeta.local)
      end

      def token_info(scheme, token)
        TokenInfo.new(scheme: scheme, expires_at: token.expires_at, scope: token.scope, refreshable: !token.refresh_token.to_s.empty?)
      end
    end
  end
end
