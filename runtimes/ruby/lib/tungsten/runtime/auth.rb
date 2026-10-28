# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

require "ipaddr"

module Tungsten
  module Runtime
    # Auth resolution: an operation's security is an OR of AND-sets of
    # scheme names; the first alternative the configured credentials fully
    # satisfy is applied. A composite profile satisfies the names in its
    # `satisfies` list when every part it needs for this request is
    # configured.
    class ClientCore
      INSECURE_HTTP_AUTH = "Refusing to send credentials over non-loopback HTTP. Use HTTPS, or set ClientOptions.allow_insecure_http = true for intentional insecure transport. Nothing was sent."

      def credential_header?(name, preserved_idempotency = false)
        (!preserved_idempotency && Util.looks_sensitive?(name)) ||
          %w[authorization proxy-authorization cookie].include?(name.downcase) ||
          Array(@api["auth"]).any? do |scheme|
            (scheme["kind"] == "api_key" && scheme["in"] == "header" && scheme["wire"].to_s.casecmp?(name)) ||
              (scheme["kind"] == "composite" && Array(scheme["parts"]).any? do |part|
                part["kind"] == "header" && part["from_config"].to_s.empty? && part["name"].to_s.casecmp?(name)
              end)
          end
      end

      def insecure_http?(url)
        return false if @allow_insecure_http

        parsed = URI.parse(url.to_s)
        host = parsed.hostname.to_s.downcase
        return false if parsed.scheme&.downcase != "http" || %w[localhost localhost.].include?(host)

        begin
          ip = IPAddr.new(host)
          loopback = ip.ipv4? ? (ip.to_i >> 24) == 127 : ip.to_i == 1
          !loopback
        rescue IPAddr::InvalidAddressError
          true
        end
      rescue URI::InvalidURIError
        false
      end
      # The headers, cookies and query parameters credentials add.
      AuthPlan = Struct.new(:headers, :cookies, :query) do
        def self.empty = new([], [], [])
      end

      # The plan, the secrets it carries, the authorization-code scheme whose
      # stored token it sends, or the remediation of an AUTH_FAILED envelope.
      AuthResult = Struct.new(:plan, :secrets, :oauth, :failed)

      Satisfier = Struct.new(:kind, :name, :parts, :config, :mutation, :scheme, :secret, :client_id,
                             :client_secret, :token_url, :scopes, keyword_init: true)

      private

      def safe_method?(method) = %w[GET HEAD OPTIONS TRACE QUERY].include?(method)

      def composite_key(part)
        case part["kind"]
        when "cookie" then part["name"]
        when "bearer" then "bearer"
        else part["equals_cookie"] || part["from_config"] || part["name"]
        end
      end

      def check_prefix(token, prefix, what)
        return nil if prefix.nil? || prefix.empty? || token.start_with?(prefix)

        "The #{what} credential must start with \"#{prefix}\"; check that the right kind of token is configured."
      end

      def skips?(part, mutation) = part["kind"] == "header" && part["mutation_only"] && !mutation

      def credential(name)
        value = @auth[name]
        case value
        when nil then UNSET
        when Hash then value.to_h { |k, v| [k.to_s, v.nil? ? nil : v.to_s] }
        else value.to_s
        end
      end

      def part_of(parts, snake, camel) = parts.key?(snake) ? parts[snake] : parts[camel]

      def try_composite(name, parts, cred, method)
        return [nil, nil, ["auth.#{name} (an object for the composite profile #{name})"]] unless cred.is_a?(Hash)

        mutation = !safe_method?(method)
        missing = []
        parts.each do |p|
          next if skips?(p, mutation)

          key = composite_key(p)
          next unless cred[key].to_s.empty?

          role =
            case p["kind"]
            when "cookie" then "cookie #{p['name']}"
            when "bearer" then "bearer token"
            else "header #{p['name']}#{p['equals_cookie'] ? " (equal to cookie #{p['equals_cookie']})" : ''}"
            end
          missing << "auth.#{name}[#{Util.js_string(key)}] (#{role} of the composite profile #{name})"
        end
        return [nil, nil, missing] unless missing.empty?

        ["composite:#{name}", Satisfier.new(kind: :composite, name: name, parts: parts, config: cred, mutation: mutation), nil]
      end

      def try_direct(scheme, cred)
        name = scheme["name"]
        if scheme["kind"] == "oauth2" && cred.is_a?(Hash)
          if part_of(cred, "flow", "flow") == "authorizationCode"
            unless scheme["authorization_code"]
              return [nil, nil, ["auth.#{name} (sets flow \"authorizationCode\", but the API descriptor has no authorizationCode flow for #{name})"]]
            end
            if part_of(cred, "client_id", "clientId").to_s.empty?
              return [nil, nil, ["auth.#{name} (a client_id for the authorizationCode flow)"]]
            end

            return ["scheme:#{name}", Satisfier.new(kind: :oauth_code, name: name, client_secret: part_of(cred, "client_secret", "clientSecret").to_s), nil]
          end
          id = part_of(cred, "client_id", "clientId")
          secret = part_of(cred, "client_secret", "clientSecret")
          if !id.nil? && !secret.nil? && scheme["token_url"].is_a?(String) && !scheme["token_url"].empty?
            return ["scheme:#{name}", Satisfier.new(kind: :oauth, name: name, token_url: scheme["token_url"], scopes: scheme["scopes"] || [],
                                                    client_id: id, client_secret: secret), nil]
          end
          code = scheme["authorization_code"] ? ", or {flow: \"authorizationCode\", client_id, client_secret?, redirect_uri?}" : ""
          return [nil, nil, ["auth.#{name} (an access token, or {client_id, client_secret} for the token URL#{code})"]]
        end
        return ["scheme:#{name}", Satisfier.new(kind: :secret, scheme: scheme, secret: cred), nil] if cred.is_a?(String) && !cred.empty?

        what =
          case scheme["kind"]
          when "http_basic" then "\"user:password\" for HTTP Basic scheme #{name}"
          when "api_key" then "API key for scheme #{name} (#{scheme['in']} #{scheme['wire']})"
          else "bearer token for scheme #{name}"
          end
        [nil, nil, ["auth.#{name} (#{what})"]]
      end

      def schemes = @api["auth"] || []

      def try_scheme(name, method)
        missing = []
        schemes.each do |s|
          next unless s["kind"] == "composite" && (s["satisfies"] || []).include?(name)

          cred = credential(s["name"])
          if UNSET.equal?(cred)
            missing << "auth.#{s['name']} (composite profile #{s['name']})"
            next
          end
          key, sat, more = try_composite(s["name"], s["parts"] || [], cred, method)
          return [key, sat, nil] if sat

          missing.concat(more)
        end
        direct = schemes.find { |s| s["name"] == name && s["kind"] != "composite" }
        if direct
          key, sat, more = try_direct(direct, credential(name))
          return [key, sat, nil] if sat

          missing.concat(more) if missing.empty?
        elsif missing.empty?
          missing << "a scheme named #{name} (the API descriptor does not define it)"
        end
        [nil, nil, missing]
      end

      def configured?(name)
        return true if @auth.key?(name)

        schemes.any? { |s| s["kind"] == "composite" && (s["satisfies"] || []).include?(name) && @auth.key?(s["name"]) }
      end

      def bearer(plan, token) = plan.headers << ["Authorization", "Bearer #{token}", true]

      # Apply one satisfier; a remediation when it cannot be applied.
      def apply(sat, result)
        plan = result.plan
        case sat.kind
        when :composite
          sat.parts.each do |p|
            next if skips?(p, sat.mutation)

            value = sat.config[composite_key(p)].to_s
            case p["kind"]
            when "cookie"
              plan.cookies << [p["name"], value]
              result.secrets << value
            when "bearer"
              problem = check_prefix(value, p["prefix"], "#{sat.name} bearer")
              return problem if problem

              bearer(plan, value)
              result.secrets << value
            else
              secret = p["from_config"].nil?
              plan.headers << [p["name"], value, secret]
              result.secrets << value if secret
            end
          end
        when :secret
          scheme = sat.scheme
          result.secrets << sat.secret
          case scheme["kind"]
          when "api_key"
            case scheme["in"]
            when "query" then plan.query << [scheme["wire"], sat.secret]
            when "cookie" then plan.cookies << [scheme["wire"], sat.secret]
            else plan.headers << [scheme["wire"], sat.secret, true]
            end
          when "http_bearer"
            problem = check_prefix(sat.secret, scheme["prefix"], scheme["name"])
            return problem if problem

            bearer(plan, sat.secret)
          when "http_basic"
            encoded = Base64.strict_encode64(sat.secret)
            result.secrets << encoded
            plan.headers << ["Authorization", "Basic #{encoded}", true]
          else bearer(plan, sat.secret)
          end
        when :oauth_code
          result.secrets << sat.client_secret unless sat.client_secret.empty?
          token, problem = stored_token(sat.name, false)
          return problem if problem

          result.secrets << token
          bearer(plan, token)
          result.oauth ||= sat.name
        when :oauth
          result.secrets << sat.client_secret
          token, problem = client_credentials_token(sat)
          return problem if problem

          result.secrets << token
          bearer(plan, token)
        end
        nil
      end

      # The credentials for an operation.
      def resolve_auth(op, method, credential_error = nil)
        empty = AuthResult.new(AuthPlan.empty, Util::Secrets.new, nil, nil)
        return empty if op.security.empty?

        best = nil
        best_touched = false
        ordered = op.security.reject(&:empty?) + op.security.select(&:empty?)
        ordered.each do |alternative|
          sats = []
          missing = []
          alternative.each do |name|
            key, sat, more = try_scheme(name, method)
            if sat.nil?
              missing.concat(more)
              next
            end
            existing = sats.find { |k, _| k == key }
            existing ? existing[1] = sat : sats << [key, sat]
          end
          if missing.empty?
            return AuthResult.new(nil, nil, nil, credential_error) if !sats.empty? && credential_error
            out = AuthResult.new(AuthPlan.empty, Util::Secrets.new, nil, nil)
            sats.each do |_, sat|
              problem = apply(sat, out)
              return AuthResult.new(nil, nil, nil, "#{problem} Operation #{op.id} was not sent.") if problem
            end
            return out
          end
          unique = missing.uniq
          touched = alternative.any? { |n| configured?(n) }
          better = best.nil? || (touched && !best_touched) || (touched == best_touched && unique.length < best.length)
          if better
            best = unique
            best_touched = touched
          end
        end
        alternatives = op.security.map { |alt| alt.empty? ? "no credentials" : alt.join(" AND ") }
        missing_text = best ? best.join("; ") : "credentials"
        AuthResult.new(nil, nil, nil,
                       "Operation #{op.id} needs #{alternatives.join(' OR ')}. Missing: #{missing_text}. " \
                       "Configure it in ClientOptions.auth and call again; the request was not sent.")
      end
    end
  end
end
