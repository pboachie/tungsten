// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"bytes"
	"fmt"
	"strings"
)

var requestIDHeaders = []string{"x-request-id", "request-id", "x-correlation-id", "x-amzn-requestid", "cf-ray"}

func requestIDOf(headers map[string]string) *string {
	for _, name := range requestIDHeaders {
		if v, ok := headers[name]; ok && v != "" {
			s := takeUnits(v, 200)
			return &s
		}
	}
	return nil
}

// mediaTypeOf is the media type without parameters, lower-cased.
func mediaTypeOf(headers map[string]string) string {
	v, ok := headers["content-type"]
	if !ok {
		return ""
	}
	media, _, _ := strings.Cut(v, ";")
	return strings.ToLower(strings.TrimSpace(media))
}

func isJSONMedia(media string) bool {
	return media == "application/json" || strings.HasSuffix(media, "+json") || media == "text/json"
}

func isJSONLMedia(media string) bool {
	switch media {
	case "application/jsonl", "application/x-jsonl", "application/ndjson", "application/x-ndjson":
		return true
	}
	return false
}

func matchResponse(responses []ResponseDescriptor, status int) *ResponseDescriptor {
	for i := range responses {
		if responses[i].Status.Kind == "exact" && responses[i].Status.Code == status {
			return &responses[i]
		}
	}
	for i := range responses {
		if responses[i].Status.Kind == "class" && responses[i].Status.Code == status/100 {
			return &responses[i]
		}
	}
	for i := range responses {
		if responses[i].Status.Kind == "default" {
			return &responses[i]
		}
	}
	return nil
}

// decodedBody is a decoded response body.
type decodedBody struct {
	// value: parsed JSON, text as a string, bytes as the Binary form; absent
	// (has false) for an empty body.
	value       any
	has         bool
	json        bool
	invalidJSON bool
	empty       bool
	jsonl       bool
	badLine     int
}

var bom = []byte{0xEF, 0xBB, 0xBF}

func decodeText(data []byte) string {
	return validUTF8(bytes.TrimPrefix(data, bom))
}

func decodeBody(data []byte, headers map[string]string, declared string) decodedBody {
	announced := mediaTypeOf(headers)
	if len(data) == 0 && !isJSONLMedia(announced) {
		return decodedBody{empty: true}
	}
	media := announced
	if media == "" {
		media = strings.ToLower(declared)
	}
	jsonlMedia := announced
	if announced == "" {
		head, _, _ := strings.Cut(media, ";")
		jsonlMedia = strings.TrimSpace(head)
	}
	if isJSONLMedia(jsonlMedia) {
		raw := decodeText(data)
		values := []any{}
		for i, line := range strings.Split(raw, "\n") {
			if strings.TrimSpace(line) == "" {
				continue
			}
			v, err := ParseJSONString(line)
			if err != nil {
				return decodedBody{value: raw, has: true, invalidJSON: true, jsonl: true, badLine: i + 1}
			}
			values = append(values, v)
		}
		return decodedBody{value: values, has: true, jsonl: true}
	}
	if isJSONMedia(media) {
		raw := decodeText(data)
		v, err := ParseJSONString(raw)
		if err != nil {
			return decodedBody{value: raw, has: true, invalidJSON: true}
		}
		return decodedBody{value: v, has: true, json: true}
	}
	if media == "" || strings.HasPrefix(media, "text/") || media == "application/problem+xml" ||
		strings.HasSuffix(media, "+xml") || media == "application/xml" {
		raw := decodeText(data)
		trimmed := strings.TrimLeft(raw, " \t\r\n\v\f")
		if media == "" && (strings.HasPrefix(trimmed, "[") || strings.HasPrefix(trimmed, "{")) {
			if v, err := ParseJSONString(raw); err == nil {
				return decodedBody{value: v, has: true, json: true}
			}
		}
		return decodedBody{value: raw, has: true}
	}
	return decodedBody{value: NewBinary(data).object(), has: true}
}

var messageFields = []string{"message", "error.message", "detail", "error_description", "title", "error"}

func serverMessage(body any, has bool) string {
	for _, field := range messageFields {
		v, ok := getPathStr(body, has, field)
		if !ok {
			continue
		}
		text, isText := v.(string)
		if !isText {
			continue
		}
		text = strings.TrimSpace(text)
		if text != "" {
			return takeUnits(text, 200)
		}
	}
	return ""
}

func errorCode(op *OperationDescriptor, body any, has bool) *string {
	if op.ErrorCodeField == "" {
		return nil
	}
	v, ok := getPathStr(body, has, op.ErrorCodeField)
	if !ok {
		return nil
	}
	switch t := v.(type) {
	case string:
		if t != "" {
			s := takeUnits(t, 200)
			return &s
		}
	case int64, float64:
		s := numberText(t)
		return &s
	}
	return nil
}

// callContext is shared by every classification of one call.
type callContext struct {
	api       *APIDescriptor
	op        *OperationDescriptor
	key       string
	hasKey    bool
	keyHeader string
	attempts  int
	check     *outcomeCheck
}

// outcomeCheck says, in words, how to find out whether an unprotected
// mutation took effect.
type outcomeCheck struct {
	call  string
	shows string
}

func refersToResponse(node any, depth int) bool {
	if depth > 32 {
		return true
	}
	switch t := node.(type) {
	case string:
		return t == "$response" || strings.HasPrefix(t, "$response.")
	case []any:
		for _, item := range t {
			if refersToResponse(item, depth+1) {
				return true
			}
		}
	case *Object:
		for _, k := range t.keys {
			if refersToResponse(t.vals[k], depth+1) {
				return true
			}
		}
	}
	return false
}

// verifyCallable: after a lost answer there is no success body, so a hook
// whose arguments reference $response cannot be called.
func verifyCallable(v *VerifyDescriptor) bool {
	return !refersToResponse(v.Args, 0)
}

func nextActionHint(ctx *callContext) *string {
	op := ctx.op
	if v := op.Agent.Verify; v != nil && v.Operation != "" && verifyCallable(v) {
		with := ""
		if o, ok := v.Args.(*Object); ok && o.Len() > 0 {
			with = " with " + strings.Join(o.Keys(), ", ")
		}
		return strPtr(fmt.Sprintf("Call %s%s to check whether %s took effect before doing anything else.", v.Operation, with, op.ID))
	}
	if op.Agent.Idempotency.Policy == IdempotencyContentIdentity {
		return strPtr(fmt.Sprintf("Call %s again with the identical body; the body is its own identity, so the server answers the original result instead of applying it twice.", op.ID))
	}
	if ctx.hasKey {
		return strPtr(fmt.Sprintf("Call %s again with the same %s value and identical arguments; the server answers the original result instead of applying it twice.", op.ID, ctx.keyHeader))
	}
	return nil
}

// changeOf is the change an operation makes, in words.
func changeOf(op *OperationDescriptor) string {
	summary := strings.TrimSpace(op.Summary)
	summary = strings.TrimSuffix(summary, ".")
	if summary == "" {
		return "the change " + op.ID + " makes"
	}
	return "the change " + jsString(summary)
}

type unknownFields struct {
	nextAction   *string
	httpStatus   *int
	code         *string
	requestID    *string
	retryAfterMs *int64
}

func hasReplayProtection(op *OperationDescriptor, hasKey bool) bool {
	return op.Agent.Idempotency.Policy == IdempotencyContentIdentity || hasKey
}

// outcomeUnknown is OUTCOME_UNKNOWN for a mutation whose effect cannot be
// known. Without replay protection a repeat can apply the effect twice, so
// the envelope never offers one.
func outcomeUnknown(ctx *callContext, cause string, fields unknownFields) *Diagnostic {
	op := ctx.op
	var rule string
	var hint *string
	switch {
	case op.Agent.Idempotency.Policy == IdempotencyContentIdentity:
		rule = "If you retry, resend the identical bytes only; the body is its own identity."
	case ctx.hasKey:
		rule = fmt.Sprintf("If you retry, reuse the SAME %s value; a new key can apply the effect twice.", ctx.keyHeader)
	case ctx.check != nil:
		rule = fmt.Sprintf("This operation has no idempotency key, so repeating it can apply the effect twice. Before calling %[1]s again, call %[2]s and check %[3]s: if it does, %[1]s took effect and must not be repeated; call it again only if it did not.",
			op.ID, ctx.check.call, ctx.check.shows)
		hint = strPtr(fmt.Sprintf("Call %s and check %s; call %s again only if it did not take effect.", ctx.check.call, ctx.check.shows, op.ID))
	default:
		change := changeOf(op)
		rule = fmt.Sprintf("This operation has no idempotency key and no registered way to verify it, so %[1]s may have taken effect and a repeat can apply the effect twice: do not retry it blindly. Check the outcome by other means if you can (look for %[2]s); if you cannot, ask whoever owns the task before calling it again.", op.ID, change)
		hint = strPtr(fmt.Sprintf("Do not retry %[1]s blindly: it may have taken effect. Check for %[2]s if you can; call %[1]s again only if it is not there, or after the task owner accepts that it may apply twice.", op.ID, change))
	}
	retryable := RetryAfterRemediation
	if hasReplayProtection(op, ctx.hasKey) {
		retryable = RetrySameKeyOnly
	}
	next := fields.nextAction
	if next == nil {
		next = hint
	}
	if next == nil {
		next = nextActionHint(ctx)
	}
	return newDiag(op.ID, OutcomeUnknown).
		remedy(fmt.Sprintf("%s The server may or may not have applied %s. %s", cause, op.ID, rule)).
		retry(retryable).
		next(next).
		statusPtr(fields.httpStatus).
		code(fields.code).
		requestID(fields.requestID).
		retryAfter(fields.retryAfterMs).
		attempts(ctx.attempts).
		build()
}

// definite: a manifest entry with one of these categories (or retryable
// never / after_remediation) is a definite answer even for an ambiguous
// status.
func definite(c Category, r Retryable) bool {
	if r == RetryNever || r == RetryAfterRemediation {
		return true
	}
	switch c {
	case ValidationFailed, MalformedRequest, RequestTooLarge, AuthFailed, NotFound, Conflict,
		PreconditionFailed, RateLimited, GateDisabled:
		return true
	}
	return false
}

func isMutation(op *OperationDescriptor) bool {
	return op.Agent.Safety != ReadOnly
}

func lookupEntry(table map[string]RemediationEntry, code *string) *RemediationEntry {
	if code == nil {
		return nil
	}
	if e, ok := table[*code]; ok {
		return &e
	}
	return nil
}

func containsInt(list []int, n int) bool {
	for _, x := range list {
		if x == n {
			return true
		}
	}
	return false
}

func optText(s string) *string {
	if s == "" {
		return nil
	}
	return &s
}

// classifyError classifies an HTTP error status.
func classifyError(ctx *callContext, status int, headers map[string]string, decoded decodedBody, retryAfter *int64) *Diagnostic {
	api, op := ctx.api, ctx.op
	requestID := requestIDOf(headers)
	base := func(b *diag) *diag {
		return b.status(status).requestID(requestID).retryAfter(retryAfter).attempts(ctx.attempts)
	}
	var code *string
	if decoded.json {
		code = errorCode(op, decoded.value, decoded.has)
	}
	if g := op.Gate; g != nil && status == g.DisabledStatus && code == nil {
		text, ok := api.Gates[g.EnvVar]
		if !ok {
			text = fmt.Sprintf("This deployment disables %s because %s is off (HTTP %d). It is a deployment setting, not a missing resource; do not retry.", op.ID, g.EnvVar, status)
		}
		return base(newDiag(op.ID, GateDisabled)).code(code).remedy(text).retry(RetryNever).build()
	}
	entry := lookupEntry(op.Agent.Remediation, code)
	if entry == nil {
		entry = lookupEntry(api.ErrorCodes, code)
	}
	media := "none"
	if !decoded.empty {
		if m := mediaTypeOf(headers); m != "" {
			media = m
		}
	}
	var nonJSON *NonJSONError
	if !decoded.json {
		for i := range api.NonJSON {
			n := &api.NonJSON[i]
			if n.Status == status && (n.Media == media || (n.Media == "none" && decoded.empty)) {
				nonJSON = n
				break
			}
		}
	}
	declared := matchResponse(op.Responses, status)
	ambiguous := containsInt(api.AmbiguousStatuses, status) ||
		(declared != nil && declared.Kind == KindAmbiguous) || (status >= 500 && status <= 599)
	definiteAnswer := false
	switch {
	case entry != nil:
		definiteAnswer = definite(entry.Category, entry.Retryable)
	case nonJSON != nil:
		definiteAnswer = definite(nonJSON.Category, nonJSON.Retryable)
	}
	if ambiguous && isMutation(op) && !definiteAnswer {
		said := ""
		switch {
		case entry != nil && entry.Text != "":
			said = entry.Text
		case nonJSON != nil && nonJSON.Text != "":
			said = nonJSON.Text
		}
		cause := fmt.Sprintf("HTTP %d leaves the outcome of this call unknown.", status)
		if said != "" {
			cause += " " + said
		}
		var next *string
		if entry != nil {
			next = optText(entry.NextAction)
		}
		s := status
		return outcomeUnknown(ctx, cause, unknownFields{
			nextAction: next, httpStatus: &s, code: code, requestID: requestID, retryAfterMs: retryAfter,
		})
	}
	var category Category
	var retryable Retryable
	var text string
	var next *string
	switch {
	case entry != nil:
		category = entry.Category
		if category == "" {
			category = categoryForStatus(status, decoded.json)
		}
		retryable = entry.Retryable
		text = entry.Text
		next = optText(entry.NextAction)
	case nonJSON != nil:
		category = nonJSON.Category
		retryable = nonJSON.Retryable
		text = nonJSON.Text
	default:
		category = categoryForStatus(status, decoded.json)
	}
	if category == OutcomeUnknown && !isMutation(op) {
		category = UpstreamUnavailable
		if retryable == "" || retryable == RetrySameKeyOnly {
			retryable = RetryAfterDelay
		}
	}
	if text == "" {
		text = GenericRemediation(category)
		if decoded.json {
			if said := serverMessage(decoded.value, decoded.has); said != "" {
				text += " Server message: " + jsString(said)
			}
		}
	}
	return base(newDiag(op.ID, category)).code(code).remedy(text).retryOpt(retryable).next(next).build()
}
