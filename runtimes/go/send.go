// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"context"
	"fmt"
	"math"
	"net/url"
	"sort"
	"strings"
	"time"
)

// maxRedirects is the number of same-origin redirects one read follows.
const maxRedirects = 5

func isRedirect(status int) bool {
	switch status {
	case 301, 302, 303, 307, 308:
		return true
	}
	return false
}

func retryableCategory(c Category) bool {
	switch c {
	case RateLimited, UpstreamUnavailable, TransportFailed, OutcomeUnknown:
		return true
	}
	return false
}

// sent is a decoded response, or an event stream whose body is unread.
type sent struct {
	response *Outcome
	stream   *streamStart
}

type streamStart struct {
	meta ResponseMeta
	body *eventBody
}

func (c *ClientCore) send(ctx context.Context, p *prepared, opts CallOptions) (*Outcome, *Error) {
	out, err := c.sendWith(ctx, p, opts, false)
	if err != nil {
		return nil, err
	}
	if out.stream != nil {
		out.stream.body.close()
		return nil, newDiag(p.op.ID, UnexpectedResponse).retry(RetryNever).
			remedy("The SDK received an event stream it did not ask for. This is a bug in the runtime; report it. If the call may have been sent, check its effect before repeating it.").err()
	}
	return out.response, nil
}

// sendWith sends with retries. With stream, a 2xx text/event-stream answer
// is returned unread.
func (c *ClientCore) sendWith(ctx context.Context, p *prepared, opts CallOptions, stream bool) (*sent, *Error) {
	op := p.op
	retries := c.retryOptions(op)
	mutation := isMutation(op)
	protected := hasReplayProtection(op, p.hasKey)
	var check *outcomeCheck
	if mutation && !protected {
		check = c.outcomeCheck(op, p.args)
	}
	timeout := c.attemptTimeout(opts)
	attempts := 0
	for {
		attempts++
		rctx := &RequestContext{
			Operation: op.ID,
			Attempt:   attempts,
			Method:    p.method,
			URL:       p.displayURL,
			Headers:   p.headers.toMap(true),
		}
		if !p.payload.empty {
			rctx.Body, rctx.HasBody = p.displayBody, true
		}
		for _, m := range c.middleware {
			m.OnRequest(rctx)
		}
		// Redirects are never followed by the HTTP client: it would forward
		// credential headers to another origin. A read follows same-origin
		// redirects here; a mutation follows none.
		target := p.url
		method := p.method
		body := &p.payload
		headers := append([]headerEntry(nil), p.headers.entries...)
		outcome := attempt(ctx, c.http, attemptRequest{url: target, method: method, headers: headers, body: body, timeout: timeout, stream: stream})
		for hops := 0; !mutation && hops < maxRedirects; hops++ {
			if outcome.kind != outcomeResponse || !isRedirect(outcome.status) {
				break
			}
			next, shown, ok := redirectTarget(p, target, outcome.headers["location"])
			if !ok {
				break
			}
			rewrite := outcome.status != 307 && outcome.status != 308 && method != "GET" && method != "HEAD"
			target = next
			if rewrite {
				method = "GET"
				body = &emptyPayload
				kept := headers[:0:0]
				for _, h := range headers {
					if !strings.EqualFold(h.name, "content-type") {
						kept = append(kept, h)
					}
				}
				headers = kept
			}
			rctx.URL = shown
			rctx.Method = method
			outcome = attempt(ctx, c.http, attemptRequest{url: target, method: method, headers: headers, body: body, timeout: timeout, stream: stream})
		}
		cctx := &callContext{api: c.api, op: op, key: p.key, hasKey: p.hasKey, keyHeader: p.keyHeader, attempts: attempts, check: check}
		result, err := c.classifyResponse(cctx, p, outcome, rctx, timeout)
		if err == nil {
			return result, nil
		}
		d := scrubDiagnostic(err.Diagnostic, &p.secrets)
		done := &Error{Diagnostic: d, Partial: err.Partial, HasPartial: err.HasPartial}
		if attempts > retries.max || !retryableCategory(d.Category) {
			return nil, done
		}
		if d.Retryable == RetryNever || d.Retryable == RetryAfterRemediation || (mutation && !protected) {
			return nil, done
		}
		if ctx.Err() != nil {
			return nil, done
		}
		var delayMs float64
		if d.RetryAfterMs != nil && retries.honorRetryAfter {
			if float64(*d.RetryAfterMs) > retries.maxMs {
				return nil, done
			}
			delayMs = float64(*d.RetryAfterMs)
		} else {
			ceiling := math.Min(retries.maxMs, retries.baseMs*math.Pow(2, float64(attempts-1)))
			r := c.random()
			switch retries.jitter {
			case JitterNone:
				delayMs = ceiling
			case JitterEqual:
				delayMs = ceiling/2 + r*ceiling/2
			default:
				delayMs = r * ceiling
			}
		}
		for _, m := range c.middleware {
			m.OnRetry(rctx, d)
		}
		timer := time.NewTimer(durationMs(delayMs))
		select {
		case <-ctx.Done():
			timer.Stop()
			return nil, done
		case <-timer.C:
		}
	}
}

// redirectTarget is the URL a read's redirect points to when it stays on
// the request's origin, with the auth query re-applied.
func redirectTarget(p *prepared, from, location string) (string, string, bool) {
	if location == "" {
		return "", "", false
	}
	base, err := url.Parse(from)
	if err != nil {
		return "", "", false
	}
	ref, err := url.Parse(location)
	if err != nil {
		return "", "", false
	}
	target := base.ResolveReference(ref)
	if !sameOrigin(base, target) || target.User != nil {
		return "", "", false
	}
	sentURL, shown := withAuthQuery(target, p.authQuery, p.hiddenQuery)
	return sentURL, shown, true
}

func defaultPort(scheme string) string {
	switch scheme {
	case "http":
		return "80"
	case "https":
		return "443"
	}
	return ""
}

func sameOrigin(a, b *url.URL) bool {
	if !strings.EqualFold(a.Scheme, b.Scheme) || !strings.EqualFold(a.Hostname(), b.Hostname()) {
		return false
	}
	pa, pb := a.Port(), b.Port()
	if pa == "" {
		pa = defaultPort(strings.ToLower(a.Scheme))
	}
	if pb == "" {
		pb = defaultPort(strings.ToLower(b.Scheme))
	}
	return pa == pb
}

func (c *ClientCore) classifyResponse(cctx *callContext, p *prepared, outcome attemptOutcome, rctx *RequestContext, timeout time.Duration) (*sent, *Error) {
	op := p.op
	mutation := isMutation(op)
	attempts := cctx.attempts
	switch outcome.kind {
	case outcomeNotSent:
		return nil, newDiag(op.ID, TransportFailed).
			remedy(fmt.Sprintf("The request could not be delivered (%s), so the server did not receive it. Check BaseURL and network access, then call again.", outcome.detail)).
			attempts(attempts).err()
	case outcomeLost:
		cause := fmt.Sprintf("The connection failed before a response arrived (%s).", outcome.detail)
		if mutation {
			return nil, newError(outcomeUnknown(cctx, cause, unknownFields{}))
		}
		return nil, newDiag(op.ID, TransportFailed).remedy(cause + " This read has no side effects; call again.").attempts(attempts).err()
	case outcomeAborted:
		if mutation && outcome.sent {
			return nil, newError(outcomeUnknown(cctx, "The caller's context cancelled the call after it was sent.", unknownFields{}))
		}
		before := ""
		if !outcome.sent {
			before = " before it was sent"
		}
		return nil, newDiag(op.ID, TransportFailed).retry(RetryNever).
			remedy(fmt.Sprintf("The caller's context cancelled the call%s.", before)).
			attempts(attempts).err()
	case outcomeTimeout:
		ms := timeout.Milliseconds()
		if mutation {
			return nil, newError(outcomeUnknown(cctx, fmt.Sprintf("No response arrived within %d ms.", ms), unknownFields{}))
		}
		return nil, newDiag(op.ID, UpstreamUnavailable).
			remedy(fmt.Sprintf("No response arrived within %d ms. This read has no side effects; call again later or with a larger `Timeout`.", ms)).
			attempts(attempts).err()
	case outcomeStreaming:
		for _, m := range c.middleware {
			m.OnResponse(rctx, &ResponseContext{Status: outcome.status, Headers: outcome.headers})
		}
		return &sent{stream: &streamStart{
			meta: ResponseMeta{Status: outcome.status, Headers: outcome.headers, RequestID: requestIDOf(outcome.headers), Attempts: attempts},
			body: outcome.stream,
		}}, nil
	}
	status, headers := outcome.status, outcome.headers
	for _, m := range c.middleware {
		m.OnResponse(rctx, &ResponseContext{Status: status, Headers: headers})
	}
	requestID := requestIDOf(headers)
	retryAfterValue, hasRetryAfter := headers["retry-after"]
	retryAfter := parseRetryAfter(retryAfterValue, hasRetryAfter, c.now())
	declared := matchResponse(op.Responses, status)
	success := (status >= 200 && status <= 299) || (declared != nil && declared.Kind == KindSuccess && status >= 100 && status <= 399)
	var hint *string
	if v := op.Agent.Verify; v != nil {
		hint = strPtr(fmt.Sprintf("Call %s to read the current state.", v.Operation))
	}
	if success && !outcome.bodyOK {
		failure := "broken"
		if outcome.bodyFailure == "timeout" {
			failure = "timeout"
		}
		if !mutation {
			return nil, newDiag(op.ID, UpstreamUnavailable).status(status).requestID(requestID).retry(RetryAfterDelay).
				remedy(fmt.Sprintf("The response body was cut off (%s). This read has no side effects; call again.", failure)).
				attempts(attempts).err()
		}
		return nil, newDiag(op.ID, UnexpectedResponse).status(status).requestID(requestID).retry(RetryNever).
			remedy(fmt.Sprintf("The server accepted %s (HTTP %d) but the response body was cut off (%s). The call took effect; do not repeat it.", op.ID, status, failure)).
			next(hint).attempts(attempts).err()
	}
	data := outcome.body
	declaredMedia := ""
	if declared != nil {
		declaredMedia = declared.MediaType
	}
	if !success {
		if status >= 300 && status <= 399 {
			to := ""
			if l, ok := headers["location"]; ok {
				to = " to " + takeUnits(l, 200)
			}
			var remediation string
			var next *string
			if mutation {
				remediation = fmt.Sprintf("The server answered with a redirect%s. Redirects are never followed on mutations; check BaseURL (scheme, host, trailing slash). Whether the call took effect is unknown only if the server applied it before redirecting.", to)
				next = hint
			} else {
				remediation = fmt.Sprintf("The server answered with a redirect%s that leaves the API's origin (or too many redirects). It is not followed, so credentials never leave the API's host; check BaseURL.", to)
			}
			return nil, newDiag(op.ID, UnexpectedResponse).status(status).requestID(requestID).remedy(remediation).
				next(next).attempts(attempts).err()
		}
		decoded := decodeBody(data, headers, declaredMedia)
		return nil, newError(classifyError(cctx, status, headers, decoded, retryAfter))
	}
	decoded := decodeBody(data, headers, declaredMedia)
	mode := c.validateResponses
	afterEffect := ""
	if mutation {
		afterEffect = " The call took effect; do not repeat it."
	}
	var problem *Diagnostic
	if decoded.invalidJSON {
		expected := "a JSON body"
		remediation := "The success response announced JSON but did not parse." + afterEffect
		if decoded.jsonl {
			expected = "a JSON Lines body"
			remediation = fmt.Sprintf("The success response announced JSON Lines but line %d did not parse.%s", decoded.badLine, afterEffect)
		}
		problem = newDiag(op.ID, UnexpectedResponse).status(status).requestID(requestID).param("response").
			expected(expected).remedy(remediation).attempts(attempts).build()
	} else if mode != ValidateOff && op.Response != nil && decoded.has {
		if issues := judge(op.Response, decoded.value); len(issues) > 0 {
			issue := issues[0]
			path := segmentStrings(issue.Path)
			pathText := formatIssuePath(issue.Path)
			kept := ""
			if mutation && mode == ValidateStrict {
				kept = " The decoded body is in the result's partial; store any value shown only once from it before anything else."
			}
			redacted := redactPaths(decoded.value, op.Agent.SensitiveResponseFields)
			received, _ := getPath(redacted, true, path)
			sensitive := false
			for _, s := range path {
				if looksSensitive(s) {
					sensitive = true
				}
			}
			problem = newDiag(op.ID, UnexpectedResponse).status(status).requestID(requestID).
				param("response" + pathText).
				received(envelopeValue(received, sensitive)).
				expected(issue.Message).
				remedy(fmt.Sprintf("The success response does not match the API description at response%s (%s).%s%s", pathText, issue.Message, afterEffect, kept)).
				attempts(attempts).build()
		}
	}
	if problem != nil {
		scrubbed := scrubDiagnostic(problem, &p.secrets)
		if mode == ValidateStrict {
			// The effect of a mutation happened: its body is never dropped,
			// since it can hold the only copy of a value.
			if mutation && !decoded.invalidJSON {
				return nil, &Error{Diagnostic: scrubbed, Partial: decoded.value, HasPartial: decoded.has}
			}
			return nil, newError(scrubbed)
		}
		c.emit(scrubbed)
	}
	return &sent{response: &Outcome{
		Value:    decoded.value,
		HasValue: decoded.has,
		Raw:      decoded.value,
		Meta:     ResponseMeta{Status: status, Headers: headers, RequestID: requestID, Attempts: attempts},
	}}, nil
}

// outcomeCheck says how to find out whether a mutation without replay
// protection took effect after its answer was lost.
func (c *ClientCore) outcomeCheck(op *OperationDescriptor, args *Object) *outcomeCheck {
	sentArgs := sentArguments(op, args)
	if hook := op.Agent.Verify; hook != nil && hook.Operation != "" && verifyCallable(hook) {
		sc := NewObject()
		sc.Set("args", withWireNames(op, args))
		hookArgs := hook.Args
		if hookArgs == nil {
			hookArgs = NewObject()
		}
		evaluated, _ := evaluateExpr(hookArgs, sc, 0)
		call := hook.Operation + withArguments(evaluated)
		var lost []string
		expect := any(NewObject())
		if _, ok := hook.Expect.(*Object); ok {
			expect = resolveLost(hook.Expect, sc, &lost, 0)
		}
		var fields []string
		if o, ok := expect.(*Object); ok {
			fields = o.Keys()
		}
		var shows string
		switch {
		case len(fields) == 0:
			shows = "whether it shows " + changeOf(op)
		case len(lost) > 0:
			var unique []string
			for _, item := range lost {
				if !containsString(unique, item) {
					unique = append(unique, item)
				}
			}
			shows = fmt.Sprintf("whether %s holds what %s creates, matching the arguments you sent%s (its %s was in the lost response)",
				strings.Join(fields, " and "), op.ID, sentArgs, strings.Join(unique, ", "))
		default:
			shows = "whether " + describePredicate(expect)
		}
		return &outcomeCheck{call: call, shows: shows}
	}
	readID, readArgs, ok := c.resourceRead(op, args)
	if !ok {
		return nil
	}
	return &outcomeCheck{call: readID + withArguments(readArgs), shows: "whether it shows " + changeOf(op)}
}

func isRootSegment(s string) bool {
	if s == "" || s == "api" {
		return true
	}
	if len(s) > 1 && (s[0] == 'v' || s[0] == 'V') {
		for _, n := range strings.Split(s[1:], ".") {
			if !allDigits(n) {
				return false
			}
		}
		return true
	}
	return false
}

// resourceRead is a registered read of the resource op changes: a GET
// whose path is the longest prefix of op's path, never the API root, whose
// required parameters are path parameters op was called with.
func (c *ClientCore) resourceRead(op *OperationDescriptor, args *Object) (string, *Object, bool) {
	if op.RPC != nil {
		return "", nil, false
	}
	known := map[string]any{}
	for _, p := range op.Params {
		if p.In != InPath {
			continue
		}
		if v, ok := args.Get(p.Name); ok && v != nil {
			known[p.Wire] = v
		}
	}
	segments := strings.Split(op.Path, "/")
	c.mu.Lock()
	ids := make([]string, 0, len(c.registry))
	for id := range c.registry {
		ids = append(ids, id)
	}
	sort.Strings(ids)
	var reads []*OperationDescriptor
	for _, id := range ids {
		r := c.registry[id]
		if r.ID != op.ID && r.Method == "GET" && r.Agent.Safety == ReadOnly && r.RPC == nil {
			reads = append(reads, r)
		}
	}
	c.mu.Unlock()
	for end := len(segments); end >= 1; end-- {
		prefix := segments[:end]
		allRoot := true
		for _, s := range prefix {
			if !isRootSegment(s) {
				allRoot = false
				break
			}
		}
		if allRoot {
			break
		}
		path := strings.Join(prefix, "/")
		for _, read := range reads {
			if read.Path != path {
				continue
			}
			params := argParams(read)
			requiredOK := true
			for _, p := range params {
				if !p.Required {
					continue
				}
				if _, ok := known[p.Wire]; p.In != InPath || !ok {
					requiredOK = false
				}
			}
			if !requiredOK {
				continue
			}
			readArgs := NewObject()
			for _, p := range params {
				if v, ok := known[p.Wire]; p.In == InPath && ok {
					readArgs.Set(p.Wire, v)
				}
			}
			return read.ID, readArgs, true
		}
	}
	return "", nil, false
}

// resolveLost resolves node against the call's arguments; references to the
// lost response show as <response...> and are recorded in lost.
func resolveLost(node any, sc scope, lost *[]string, depth int) any {
	if depth > 32 {
		return nil
	}
	switch t := node.(type) {
	case string:
		if strings.HasPrefix(t, "$response") {
			rest := strings.TrimPrefix(strings.TrimPrefix(t, "$response"), ".")
			if rest == "" {
				rest = "body"
			}
			*lost = append(*lost, rest)
			return "<" + t[1:] + ">"
		}
		if strings.HasPrefix(t, "$") {
			if v, ok := resolveRef(t, sc); ok {
				return v
			}
			return "<" + t[1:] + ">"
		}
		return t
	case []any:
		out := make([]any, len(t))
		for i, item := range t {
			out[i] = resolveLost(item, sc, lost, depth+1)
		}
		return out
	case *Object:
		out := NewObject()
		for _, k := range t.keys {
			out.Set(k, resolveLost(t.vals[k], sc, lost, depth+1))
		}
		return out
	}
	return node
}
