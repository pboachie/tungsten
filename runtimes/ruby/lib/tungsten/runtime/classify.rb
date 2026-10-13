# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # Decoding answers and classifying failures into the envelope.
    module Classify
      REQUEST_ID_HEADERS = %w[x-request-id request-id x-correlation-id x-amzn-requestid cf-ray].freeze
      MESSAGE_FIELDS = %w[message error.message detail error_description title error].freeze

      # A decoded body: `value` (parsed JSON, text, or a Binary; UNSET for an
      # empty body), and what it was.
      Decoded = Data.define(:value, :json, :invalid_json, :empty, :jsonl, :bad_line) do
        def initialize(value:, json: false, invalid_json: false, empty: false, jsonl: false, bad_line: 0) = super
        def present? = !UNSET.equal?(value)
      end

      # Everything one call's classification shares: the operation, its key
      # and header, the attempts so far and how to check a lost mutation.
      CallContext = Struct.new(:api, :op, :key, :key_header, :attempts, :check, keyword_init: true) do
        def key? = !key.nil?
      end

      # How to find out whether an unprotected mutation took effect: the call
      # to make and what to look for, in words.
      OutcomeCheck = Data.define(:call, :shows)

      module_function

      def request_id(headers)
        REQUEST_ID_HEADERS.each do |name|
          v = headers[name]
          return Json.take_units(v, 200) if v.is_a?(String) && !v.empty?
        end
        nil
      end

      def media_type(headers) = headers.fetch("content-type", "").split(";").first.to_s.strip.downcase

      def json_media?(media) = media == "application/json" || media.end_with?("+json") || media == "text/json"

      def jsonl_media?(media) = %w[application/jsonl application/x-jsonl application/ndjson application/x-ndjson].include?(media)

      def match_response(responses, status)
        responses.find { |r| r.status == status } ||
          responses.find { |r| r.status.is_a?(String) && r.status.match?(/\A[1-5]XX\z/) && r.status[0].to_i == status / 100 } ||
          responses.find { |r| r.status == "default" }
      end

      def decode_text(data)
        text = data.b.delete_prefix("\xEF\xBB\xBF".b).force_encoding(Encoding::UTF_8)
        text.valid_encoding? ? text : text.scrub("\uFFFD")
      end

      def decode_body(data, headers, declared)
        announced = media_type(headers)
        return Decoded.new(value: UNSET, empty: true) if data.empty? && !jsonl_media?(announced)

        media = announced.empty? ? declared.to_s.downcase : announced
        jsonl_media = announced.empty? ? media.split(";").first.to_s.strip : announced
        if jsonl_media?(jsonl_media)
          raw = decode_text(data)
          values = []
          raw.split("\n", -1).each_with_index do |line, i|
            next if line.strip.empty?

            v = Json.try_parse(line)
            return Decoded.new(value: raw, invalid_json: true, jsonl: true, bad_line: i + 1) if UNSET.equal?(v)

            values << v
          end
          return Decoded.new(value: values, jsonl: true)
        end
        if json_media?(media)
          raw = decode_text(data)
          v = Json.try_parse(raw)
          return UNSET.equal?(v) ? Decoded.new(value: raw, invalid_json: true) : Decoded.new(value: v, json: true)
        end
        if media.empty? || media.start_with?("text/") || media == "application/problem+xml" || media.end_with?("+xml") ||
           media == "application/xml"
          raw = decode_text(data)
          if media.empty? && raw.lstrip.start_with?("[", "{")
            v = Json.try_parse(raw)
            return Decoded.new(value: v, json: true) unless UNSET.equal?(v)
          end
          return Decoded.new(value: raw)
        end
        Decoded.new(value: Binary.new(data: data))
      end

      def server_message(body)
        MESSAGE_FIELDS.each do |field|
          v = Util.get_path_str(body, field)
          next unless v.is_a?(String)

          text = v.strip
          return Json.take_units(text, 200) unless text.empty?
        end
        nil
      end

      def error_code(op, body)
        return nil if op.error_code_field.nil? || op.error_code_field.empty?

        v = Util.get_path_str(body, op.error_code_field)
        case v
        when String then v.empty? ? nil : Json.take_units(v, 200)
        when Integer, Float then Json.number_text(v)
        end
      end

      def refers_to_response?(node, depth = 0)
        return true if depth > 32

        case node
        when String then node == "$response" || node.start_with?("$response.")
        when Array then node.any? { |n| refers_to_response?(n, depth + 1) }
        when Hash then node.each_value.any? { |n| refers_to_response?(n, depth + 1) }
        else false
        end
      end

      # After a lost answer there is no success body, so a hook whose
      # arguments reference $response cannot be called.
      def verify_callable?(verify) = !refers_to_response?(verify["args"])

      def next_action_hint(ctx)
        op = ctx.op
        v = op.agent["verify"]
        if v && v["operation"].to_s != "" && verify_callable?(v)
          args = v["args"]
          with = args.is_a?(Hash) && !args.empty? ? " with #{args.keys.join(', ')}" : ""
          return "Call #{v['operation']}#{with} to check whether #{op.id} took effect before doing anything else."
        end
        if op.agent["idempotency"]["policy"] == "content_identity"
          return "Call #{op.id} again with the identical body; the body is its own identity, so the server answers the original result instead of applying it twice."
        end
        return nil unless ctx.key?

        "Call #{op.id} again with the same #{ctx.key_header} value and identical arguments; the server answers the original result instead of applying it twice."
      end

      # The change an operation makes, in words.
      def change_of(op)
        summary = op.summary.strip.delete_suffix(".")
        summary.empty? ? "the change #{op.id} makes" : "the change #{Util.js_string(summary)}"
      end

      def replay_protection?(op, has_key) = op.agent["idempotency"]["policy"] == "content_identity" || has_key

      # OUTCOME_UNKNOWN for a mutation whose effect cannot be known. Without
      # replay protection a repeat can apply the effect twice, so the
      # envelope never offers one.
      def outcome_unknown(ctx, cause, next_action: nil, http_status: nil, code: nil, request_id: nil, retry_after_ms: nil)
        op = ctx.op
        hint = nil
        rule =
          if op.agent["idempotency"]["policy"] == "content_identity"
            "If you retry, resend the identical bytes only; the body is its own identity."
          elsif ctx.key?
            "If you retry, reuse the SAME #{ctx.key_header} value; a new key can apply the effect twice."
          elsif ctx.check
            c = ctx.check
            hint = "Call #{c.call} and check #{c.shows}; call #{op.id} again only if it did not take effect."
            "This operation has no idempotency key, so repeating it can apply the effect twice. Before calling #{op.id} again, " \
              "call #{c.call} and check #{c.shows}: if it does, #{op.id} took effect and must not be repeated; call it again only if it did not."
          else
            change = change_of(op)
            hint = "Do not retry #{op.id} blindly: it may have taken effect. Check for #{change} if you can; call #{op.id} " \
                   "again only if it is not there, or after the task owner accepts that it may apply twice."
            "This operation has no idempotency key and no registered way to verify it, so #{op.id} may have taken effect and " \
              "a repeat can apply the effect twice: do not retry it blindly. Check the outcome by other means if you can " \
              "(look for #{change}); if you cannot, ask whoever owns the task before calling it again."
          end
        retryable = replay_protection?(op, ctx.key?) ? "same_key_only" : "after_remediation"
        Runtime.diag(op.id, "OUTCOME_UNKNOWN")
               .remedy("#{cause} The server may or may not have applied #{op.id}. #{rule}")
               .retrying(retryable).next_action(next_action || hint || next_action_hint(ctx))
               .status(http_status).code(code).request_id(request_id).retry_after_ms(retry_after_ms)
               .attempts(ctx.attempts).build
      end

      DEFINITE = %w[VALIDATION_FAILED MALFORMED_REQUEST REQUEST_TOO_LARGE AUTH_FAILED NOT_FOUND CONFLICT PRECONDITION_FAILED
                    RATE_LIMITED GATE_DISABLED].freeze

      # A manifest entry with one of these categories (or retryable never /
      # after_remediation) is a definite answer even for an ambiguous status.
      def definite?(category, retryable) = %w[never after_remediation].include?(retryable) || DEFINITE.include?(category)

      # The envelope of an HTTP error status.
      def classify_error(ctx, status, headers, decoded, retry_after)
        api = ctx.api
        op = ctx.op
        rid = request_id(headers)
        base = ->(d) { d.status(status).request_id(rid).retry_after_ms(retry_after).attempts(ctx.attempts) }
        code = decoded.json ? error_code(op, decoded.value) : nil
        gate = op.gate
        if gate && status == gate["disabled_status"] && code.nil?
          text = (api["gates"] || {})[gate["env_var"]] ||
                 "This deployment disables #{op.id} because #{gate['env_var']} is off (HTTP #{status}). It is a deployment setting, not a missing resource; do not retry."
          return base.call(Runtime.diag(op.id, "GATE_DISABLED")).code(code).remedy(text).retrying("never").build
        end
        entry = code && (op.agent["remediation"][code] || (api["error_codes"] || {})[code])
        media = "none"
        media = media_type(headers).then { |m| m.empty? ? "none" : m } unless decoded.empty
        non_json = nil
        unless decoded.json
          non_json = (api["non_json"] || []).find do |n|
            n["status"] == status && (n["media"] == media || (n["media"] == "none" && decoded.empty))
          end
        end
        declared = match_response(op.responses, status)
        ambiguous = (api["ambiguous_statuses"] || []).include?(status) || declared&.kind == "ambiguous" || status.between?(500, 599)
        definite = (entry && definite?(entry["category"], entry["retryable"])) || (!entry && non_json && definite?(non_json["category"], non_json["retryable"]))
        if ambiguous && op.mutation? && !definite
          said = entry && !entry["text"].to_s.empty? ? entry["text"] : (non_json && !non_json["text"].to_s.empty? ? non_json["text"] : nil)
          cause = "HTTP #{status} leaves the outcome of this call unknown."
          cause += " #{said}" if said
          return outcome_unknown(ctx, cause, next_action: entry && entry["next_action"], http_status: status, code: code,
                                             request_id: rid, retry_after_ms: retry_after)
        end
        if entry
          category = entry["category"] || Runtime.category_for_status(status, decoded.json)
          retryable = entry["retryable"]
          text = entry["text"]
          next_action = entry["next_action"]
        elsif non_json
          category = non_json["category"]
          retryable = non_json["retryable"]
          text = non_json["text"]
        else
          category = Runtime.category_for_status(status, decoded.json)
        end
        if category == "OUTCOME_UNKNOWN" && !op.mutation?
          category = "UPSTREAM_UNAVAILABLE"
          retryable = "after_delay" if retryable.nil? || retryable == "same_key_only"
        end
        if text.nil? || text.empty?
          text = Runtime.generic_remediation(category)
          if decoded.json && (said = server_message(decoded.value))
            text += " Server message: #{Util.js_string(said)}"
          end
        end
        base.call(Runtime.diag(op.id, category)).code(code).remedy(text).retrying_opt(retryable).next_action(next_action).build
      end
    end
  end
end
