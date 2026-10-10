// SPDX-License-Identifier: Apache-2.0

package tungsten

// DefaultRetryable is the default retryable of a category.
func DefaultRetryable(c Category) Retryable {
	switch c {
	case RateLimited, UpstreamUnavailable, TransportFailed:
		return RetryAfterDelay
	case OutcomeUnknown:
		return RetrySameKeyOnly
	}
	return RetryNever
}

// GenericRemediation is the remediation used when no manifest entry
// applies.
func GenericRemediation(c Category) string {
	switch c {
	case ValidationFailed:
		return "The server rejected the arguments. Fix the parameter the API error names and call again; the same request fails the same way."
	case MalformedRequest:
		return "The server could not parse the request. Check the format of path and query parameters (for example canonical UUIDs); do not retry unchanged."
	case RequestTooLarge:
		return "The request body is larger than the server accepts. Send a smaller body; do not retry unchanged."
	case AuthFailed:
		return "The credential is missing, expired, revoked or lacks permission. Do not retry; fix the credential configured in ClientOptions.auth."
	case NotFound:
		return "The resource does not exist or is not visible to this credential. Check the identifiers; do not retry unchanged."
	case Conflict:
		return "The request conflicts with the resource's current state. Read the current state before deciding what to do; do not retry unchanged."
	case PreconditionFailed:
		return "A precondition of this operation is not met (account, billing or resource state). Resolve it first; do not retry unchanged."
	case RateLimited:
		return "Rate limited. Wait retry_after_ms (or a few seconds when it is null) before calling again."
	case UpstreamUnavailable:
		return "The service is temporarily unavailable. Wait, then call again."
	case OutcomeUnknown:
		return "The server may or may not have applied this call; do not retry it blindly. Check whether it took effect before doing anything else."
	case TransportFailed:
		return "The request could not be delivered (DNS, TLS or connection failure), so the server did not receive it. Check BaseURL and network access, then call again."
	case ConfirmationRequired:
		return "This operation needs confirmation: call preview(...) and pass its confirmation_token."
	case GateDisabled:
		return "This operation is disabled on this deployment. It is a deployment setting; do not retry."
	case UnexpectedResponse:
		return "The server's response did not match the API description. Do not retry blindly; report it."
	}
	return "The call failed."
}

// categoryForStatus is the category of an HTTP error status without a
// manifest entry.
func categoryForStatus(status int, jsonBody bool) Category {
	switch status {
	case 401, 403, 407:
		return AuthFailed
	case 404, 410:
		return NotFound
	case 409:
		return Conflict
	case 402, 412, 428:
		return PreconditionFailed
	case 413:
		return RequestTooLarge
	case 422:
		return ValidationFailed
	case 429:
		return RateLimited
	case 408, 502, 503, 504:
		return UpstreamUnavailable
	}
	switch {
	case status >= 500 && status <= 599:
		return UpstreamUnavailable
	case status >= 400 && status <= 499:
		if jsonBody {
			return ValidationFailed
		}
		return MalformedRequest
	}
	return UnexpectedResponse
}

// diag builds an envelope with every member present.
type diag struct {
	d             Diagnostic
	hasRemedy     bool
	hasRetryable  bool
	receivedValue any
}

func newDiag(operation string, c Category) *diag {
	return &diag{d: Diagnostic{Operation: operation, Category: c}}
}

func strPtr(s string) *string { return &s }

func (b *diag) status(code int) *diag {
	b.d.HTTPStatus = &code
	return b
}

func (b *diag) statusPtr(code *int) *diag {
	b.d.HTTPStatus = code
	return b
}

func (b *diag) code(code *string) *diag {
	b.d.Code = code
	return b
}

func (b *diag) param(path string) *diag {
	b.d.FailedParameter = &path
	return b
}

func (b *diag) received(v any) *diag {
	b.receivedValue = v
	return b
}

func (b *diag) expected(text string) *diag {
	b.d.Expected = &text
	return b
}

func (b *diag) remedy(text string) *diag {
	b.d.Remediation = text
	b.hasRemedy = true
	return b
}

func (b *diag) retry(r Retryable) *diag {
	b.d.Retryable = r
	b.hasRetryable = true
	return b
}

func (b *diag) retryOpt(r Retryable) *diag {
	if r != "" {
		return b.retry(r)
	}
	return b
}

func (b *diag) retryAfter(ms *int64) *diag {
	b.d.RetryAfterMs = ms
	return b
}

func (b *diag) next(text *string) *diag {
	b.d.NextAction = text
	return b
}

func (b *diag) requestID(id *string) *diag {
	b.d.RequestID = id
	return b
}

func (b *diag) attempts(n int) *diag {
	b.d.Attempts = n
	return b
}

func (b *diag) build() *Diagnostic {
	out := b.d
	out.ReceivedValue = b.receivedValue
	if !b.hasRemedy {
		out.Remediation = GenericRemediation(out.Category)
	}
	if !b.hasRetryable {
		out.Retryable = DefaultRetryable(out.Category)
	}
	return &out
}

func (b *diag) err() *Error { return newError(b.build()) }

func scrubPtr(p *string, secrets *secretSet) *string {
	if p == nil {
		return nil
	}
	s := scrubText(*p, secrets)
	return &s
}

// scrubDiagnostic replaces every occurrence of the secrets in the
// envelope's text members: a last line of defence.
func scrubDiagnostic(d *Diagnostic, secrets *secretSet) *Diagnostic {
	if secrets == nil || secrets.empty() {
		return d
	}
	out := *d
	out.Code = scrubPtr(d.Code, secrets)
	out.FailedParameter = scrubPtr(d.FailedParameter, secrets)
	out.ReceivedValue = scrubValue(d.ReceivedValue, secrets)
	out.Expected = scrubPtr(d.Expected, secrets)
	out.Remediation = scrubText(d.Remediation, secrets)
	out.NextAction = scrubPtr(d.NextAction, secrets)
	out.RequestID = scrubPtr(d.RequestID, secrets)
	return &out
}
