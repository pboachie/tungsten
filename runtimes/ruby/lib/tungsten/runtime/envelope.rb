# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # The closed set of error categories shared by every tungsten runtime.
    CATEGORIES = %w[
      VALIDATION_FAILED MALFORMED_REQUEST REQUEST_TOO_LARGE AUTH_FAILED NOT_FOUND CONFLICT
      PRECONDITION_FAILED RATE_LIMITED UPSTREAM_UNAVAILABLE OUTCOME_UNKNOWN TRANSPORT_FAILED
      CONFIRMATION_REQUIRED GATE_DISABLED UNEXPECTED_RESPONSE
    ].freeze

    # Whether and how a failed call may be repeated.
    RETRYABLE = %w[never after_delay same_key_only after_remediation].freeze

    # Written wherever a secret or sensitive value would appear.
    REDACTED = "<redacted>"

    # The error envelope (planning/06): `to_h` is exactly the fourteen
    # members of specs/envelope.schema.json in order, every member present
    # (nil when it has no value), identical across the runtimes.
    Diagnostic = Data.define(
      :category, :operation, :http_status, :code, :failed_parameter, :received_value, :expected,
      :remediation, :retryable, :retry_after_ms, :next_action, :request_id, :attempts
    ) do
      def to_h
        {
          "status" => "error",
          "category" => category,
          "operation" => operation,
          "http_status" => http_status,
          "code" => code,
          "failed_parameter" => failed_parameter,
          "received_value" => received_value,
          "expected" => expected,
          "remediation" => remediation,
          "retryable" => retryable,
          "retry_after_ms" => retry_after_ms,
          "next_action" => next_action,
          "request_id" => request_id,
          "trace" => { "attempts" => attempts }
        }
      end

      def to_json(*) = Json.text(to_h)

      # "<category> in <operation>: <remediation>".
      def message = "#{category} in #{operation}: #{remediation}"
      alias_method :to_s, :message
    end

    # The default `retryable` of a category.
    def self.default_retryable(category)
      case category
      when "RATE_LIMITED", "UPSTREAM_UNAVAILABLE", "TRANSPORT_FAILED" then "after_delay"
      when "OUTCOME_UNKNOWN" then "same_key_only"
      else "never"
      end
    end

    GENERIC_REMEDIATION = {
      "VALIDATION_FAILED" => "The server rejected the arguments. Fix the parameter the API error names and call again; the same request fails the same way.",
      "MALFORMED_REQUEST" => "The server could not parse the request. Check the format of path and query parameters (for example canonical UUIDs); do not retry unchanged.",
      "REQUEST_TOO_LARGE" => "The request body is larger than the server accepts. Send a smaller body; do not retry unchanged.",
      "AUTH_FAILED" => "The credential is missing, expired, revoked or lacks permission. Do not retry; fix the credential configured in ClientOptions.auth.",
      "NOT_FOUND" => "The resource does not exist or is not visible to this credential. Check the identifiers; do not retry unchanged.",
      "CONFLICT" => "The request conflicts with the resource's current state. Read the current state before deciding what to do; do not retry unchanged.",
      "PRECONDITION_FAILED" => "A precondition of this operation is not met (account, billing or resource state). Resolve it first; do not retry unchanged.",
      "RATE_LIMITED" => "Rate limited. Wait retry_after_ms (or a few seconds when it is null) before calling again.",
      "UPSTREAM_UNAVAILABLE" => "The service is temporarily unavailable. Wait, then call again.",
      "OUTCOME_UNKNOWN" => "The server may or may not have applied this call; do not retry it blindly. Check whether it took effect before doing anything else.",
      "TRANSPORT_FAILED" => "The request could not be delivered (DNS, TLS or connection failure), so the server did not receive it. Check base_url and network access, then call again.",
      "CONFIRMATION_REQUIRED" => "This operation needs confirmation: call preview(...) and pass its confirmation_token.",
      "GATE_DISABLED" => "This operation is disabled on this deployment. It is a deployment setting; do not retry.",
      "UNEXPECTED_RESPONSE" => "The server's response did not match the API description. Do not retry blindly; report it."
    }.freeze

    # The remediation used when no manifest entry applies.
    def self.generic_remediation(category) = GENERIC_REMEDIATION.fetch(category, "The call failed.")

    # The category of an HTTP error status without a manifest entry.
    def self.category_for_status(status, json_body)
      case status
      when 401, 403, 407 then "AUTH_FAILED"
      when 404, 410 then "NOT_FOUND"
      when 409 then "CONFLICT"
      when 402, 412, 428 then "PRECONDITION_FAILED"
      when 413 then "REQUEST_TOO_LARGE"
      when 422 then "VALIDATION_FAILED"
      when 429 then "RATE_LIMITED"
      when 408, 502, 503, 504 then "UPSTREAM_UNAVAILABLE"
      when 500..599 then "UPSTREAM_UNAVAILABLE"
      when 400..499 then json_body ? "VALIDATION_FAILED" : "MALFORMED_REQUEST"
      else "UNEXPECTED_RESPONSE"
      end
    end

    # Builds an envelope member by member; `build` fills the remediation
    # and `retryable` a category implies when they were not set.
    class Diag
      def initialize(operation, category)
        @fields = { operation: operation, category: category, http_status: nil, code: nil, failed_parameter: nil,
                    received_value: nil, expected: nil, remediation: nil, retryable: nil, retry_after_ms: nil,
                    next_action: nil, request_id: nil, attempts: 0 }
      end

      %i[http_status code failed_parameter received_value expected remediation retry_after_ms next_action
         request_id attempts].each do |name|
        define_method(name) do |value|
          @fields[name] = value
          self
        end
      end

      def status(code) = http_status(code)
      def param(path) = failed_parameter(path)
      def received(value) = received_value(value)
      def remedy(text) = remediation(text)

      def retrying(value)
        @fields[:retryable] = value
        self
      end

      # `retrying` unless the value is nil.
      def retrying_opt(value) = value.nil? ? self : retrying(value)

      def build
        f = @fields.dup
        f[:remediation] ||= Runtime.generic_remediation(f[:category])
        f[:retryable] ||= Runtime.default_retryable(f[:category])
        Diagnostic.new(**f)
      end

      def err = Err.new(error: build, partial: UNSET)
    end

    # A diagnostic builder.
    def self.diag(operation, category) = Diag.new(operation, category)

    # What the HTTP answer of a call said: the status, the headers (names
    # lower-cased; repeated values joined with ", ", set-cookie keeps its
    # last), the request id the server gave and the attempts made.
    ResponseMeta = Data.define(:status, :headers, :request_id, :attempts) do
      def self.local = new(status: 0, headers: {}, request_id: nil, attempts: 0)
    end

    # The result of an operation's verification hook: `checked` false when
    # it could not run (then `error` says why); `passed` when its
    # expectation held; `observed` the read's body (sensitive fields
    # redacted); `timed_out` when polling for its terminal state ran out of
    # budget.
    Verification = Data.define(:checked, :passed, :observed, :timed_out, :error) do
      def self.unchecked(error = nil) = new(checked: false, passed: false, observed: UNSET, timed_out: false, error: error)

      # The verification as the TypeScript runtime shapes it.
      def to_h
        out = { "checked" => checked, "passed" => passed, "observed" => UNSET.equal?(observed) ? nil : observed }
        out["timed_out"] = true if timed_out
        out["error"] = error.to_h if error
        out
      end
    end

    # A successful call. `value` is the typed value (a model, a page, ...),
    # `raw` the decoded wire value it came from (nil for a success without a
    # body), `meta` the HTTP answer, `verification` the verification
    # result when the call asked for one.
    Ok = Data.define(:value, :raw, :meta, :verification) do
      def initialize(value:, meta:, raw: value, verification: nil) = super
      def ok? = true
      def error = nil
      def partial = UNSET
    end

    # A failed call: `error` is the envelope (a Diagnostic); `partial` what
    # the failed call or macro already produced when its effect happened (a
    # mutation's body that failed strict validation, a macro's completed
    # step results by name), else UNSET. A partial can hold values the API
    # shows only once: store them before acting on the error.
    Err = Data.define(:error, :partial) do
      def initialize(error:, partial: UNSET) = super
      def ok? = false
      def value = nil
      def raw = nil
      def meta = nil
      def verification = nil
      def partial? = !UNSET.equal?(partial)
    end

    # A request rendered without sending it; secrets show as <redacted>.
    RenderedRequest = Data.define(:http_method, :url, :headers, :body) do
      def to_h = { "method" => http_method, "url" => url, "headers" => headers, "body" => body }
    end

    # One step of a macro preview: `request` is nil when the step's
    # arguments (or a bytes or multipart body) come from an earlier result.
    MacroStepPreview = Data.define(:step, :kind, :operation, :as, :safety, :request, :effects) do
      def to_h
        { "step" => step, "kind" => kind, "operation" => operation, "as" => as, "safety" => safety,
          "request" => request&.to_h, "effects" => effects }
      end
    end

    # What `preview` returns: the request (for a macro, its first step's),
    # the side effects in words, and for a mutating operation a
    # confirmation token bound to the exact arguments for `expires_in_ms`.
    PreviewResult = Data.define(
      :operation, :safety, :request, :effects, :confirmation_token, :expires_in_ms, :server_preview, :steps
    ) do
      def to_h
        out = { "operation" => operation, "safety" => safety, "request" => request.to_h, "effects" => effects,
                "confirmation_token" => confirmation_token, "expires_in_ms" => expires_in_ms }
        out["server_preview"] = server_preview unless UNSET.equal?(server_preview)
        out["steps"] = steps.map(&:to_h) if steps
        out
      end
    end

    # One page of a paginated operation: its items, the whole body and the
    # cursor, offset, page number or URL of the next page (nil at the end).
    Page = Data.define(:items, :body, :next_page)

    # One event of a stream: its decoded `data` (`value`; `raw` is the JSON
    # it came from), the event name (`message` when it has none), the last
    # event id and retry value the stream has set so far, the answer of the
    # connection it arrived on and the reconnects made up to it.
    StreamEvent = Data.define(:value, :raw, :event, :id, :retry_ms, :meta, :reconnects)
  end
end
