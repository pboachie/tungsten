// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"context"
	"fmt"
	"net/url"
	"sort"
	"strings"
	"sync"
)

type purpose int

const (
	purposeCall purpose = iota
	purposePreview
	purposeServerPreview
	purposeMacroPreview
)

// macroClaim is a macro run's one-time spend of its confirmation token: the
// first step about to send claims it.
type macroClaim struct {
	name    string
	token   string
	bind    string
	hasBind bool
	once    sync.Once
	err     *Error
}

func (m *macroClaim) claim(c *ClientCore) *Error {
	if m.token == "" {
		return nil
	}
	claimed := false
	m.once.Do(func() {
		claimed = true
		m.err = c.claimToken(m.name, m.token, m.bind, m.hasBind, "the macro's preview(...)")
	})
	if claimed {
		return m.err
	}
	return nil
}

type confirmTarget struct {
	id          string
	subject     string
	previewCall string
}

// prepared is a request ready to send, with its redacted rendering.
type prepared struct {
	op          *OperationDescriptor
	args        *Object
	method      string
	url         string
	displayURL  string
	headers     headerBag
	payload     payload
	displayBody any
	key         string
	hasKey      bool
	keyHeader   string
	authQuery   []pair
	hiddenQuery []string
	secrets     secretSet
	oauth       string
}

func keyPath(name string) []PathSegment { return []PathSegment{name} }

func isDotSegment(segment string) bool {
	if segment == "" {
		return true
	}
	rest := strings.ToLower(segment)
	count := 0
	for count < 3 {
		if r, ok := strings.CutPrefix(rest, "%2e"); ok {
			rest = r
		} else if r, ok := strings.CutPrefix(rest, "."); ok {
			rest = r
		} else {
			break
		}
		count++
	}
	return rest == "" && count >= 1 && count <= 2
}

// setQueryParam sets the query parameter name to value, replacing existing
// ones (URLSearchParams.set).
func setQueryParam(u *url.URL, name, value string) {
	pairs := queryPairs(u)
	index := -1
	for i, p := range pairs {
		if p.k == name {
			index = i
			break
		}
	}
	if index < 0 {
		pairs = append(pairs, pair{name, value})
	} else {
		pairs[index].v = value
		kept := pairs[:0]
		seen := false
		for _, p := range pairs {
			if p.k == name {
				if seen {
					continue
				}
				seen = true
			}
			kept = append(kept, p)
		}
		pairs = kept
	}
	parts := make([]string, len(pairs))
	for i, p := range pairs {
		parts[i] = encodeFormComponent(p.k) + "=" + encodeFormComponent(p.v)
	}
	u.RawQuery = strings.Join(parts, "&")
}

func queryPairs(u *url.URL) []pair {
	var out []pair
	if u.RawQuery == "" {
		return out
	}
	for _, part := range strings.Split(u.RawQuery, "&") {
		if part == "" {
			continue
		}
		k, v, _ := strings.Cut(part, "=")
		k, _ = url.QueryUnescape(k)
		v, _ = url.QueryUnescape(v)
		out = append(out, pair{k, v})
	}
	return out
}

func hasQueryParam(u *url.URL, name string) bool {
	for _, p := range queryPairs(u) {
		if p.k == name {
			return true
		}
	}
	return false
}

// withAuthQuery is the URL of a followed redirect, as sent and as shown.
func withAuthQuery(target *url.URL, authQuery []pair, hidden []string) (string, string) {
	sent := *target
	for _, p := range authQuery {
		if !hasQueryParam(&sent, p.k) {
			setQueryParam(&sent, p.k, p.v)
		}
	}
	shown := sent
	for _, name := range hidden {
		if hasQueryParam(&shown, name) {
			setQueryParam(&shown, name, Redacted)
		}
	}
	return sent.String(), shown.String()
}

func (c *ClientCore) validation(op *OperationDescriptor, path []PathSegment, value any, present bool, expected, remediation string) *Error {
	var received any
	if present {
		received = envelopeValue(redactBelow(op, path, value), sensitiveArg(op, path))
	} else {
		received = envelopeValue(nil, sensitiveArg(op, path))
	}
	return newDiag(op.ID, ValidationFailed).
		param(argumentPath(op, path)).
		received(received).
		expected(expected).
		remedy(remediation).
		err()
}

func (c *ClientCore) validateArgs(op *OperationDescriptor, args *Object, placeholders bool) (*Object, *Error) {
	params := argParams(op)
	for _, p := range params {
		v, ok := args.Get(p.Name)
		if p.Required && (!ok || v == nil) {
			return nil, c.validation(op, keyPath(p.Name), v, ok,
				fmt.Sprintf("a value for the required %s parameter %s", p.In, p.Wire),
				fmt.Sprintf("Pass %s; %s cannot be called without it.", p.Name, op.ID))
		}
	}
	body := op.Body
	if body != nil && body.Required && body.Arg != "" && !args.Has(body.Arg) {
		return nil, c.validation(op, keyPath(body.Arg), nil, false, "a request body",
			fmt.Sprintf("Pass the request body as %s.", body.Arg))
	}
	if op.Request != nil {
		issues, panicked := runValidator(op.Request, args)
		if panicked {
			return nil, newDiag(op.ID, UnexpectedResponse).retry(RetryNever).
				remedy("The SDK failed while building or sending the request (the request validator panicked). This is a bug in the generated SDK or the runtime; report it. Nothing was sent.").err()
		}
		if len(issues) == 0 {
			return args, nil
		}
		pending := func(path []PathSegment) bool {
			if !placeholders {
				return false
			}
			for i := range path {
				if v, ok := getPath(args, true, segmentStrings(path[:i+1])); ok && containsPlaceholder(v) {
					return true
				}
			}
			return false
		}
		var issue *Issue
		if placeholders {
			for i := range issues {
				if !pending(issues[i].Path) {
					issue = &issues[i]
					break
				}
			}
			if issue == nil {
				return args, nil
			}
		} else {
			issue = &issues[0]
		}
		shown := argumentPath(op, issue.Path)
		v, ok := getPath(args, true, segmentStrings(issue.Path))
		return nil, c.validation(op, issue.Path, v, ok, issue.Message,
			fmt.Sprintf("Fix %s (%s) and call again.", shown, issue.Message))
	}
	// Without a validator, unknown keys are rejected so a misspelt argument
	// is never dropped (rpc methods without a body take free-form args).
	if op.RPC != nil && body == nil {
		return args, nil
	}
	allowed := map[string]bool{}
	for _, p := range params {
		allowed[p.Name] = true
	}
	if body != nil {
		if body.Arg != "" {
			allowed[body.Arg] = true
		} else {
			for _, f := range body.Fields {
				allowed[f.Arg] = true
			}
		}
	}
	for _, k := range args.keys {
		if allowed[k] {
			continue
		}
		names := make([]string, 0, len(allowed))
		for n := range allowed {
			names = append(names, n)
		}
		sort.Strings(names)
		if len(names) > 20 {
			names = names[:20]
		}
		list := "no arguments"
		if len(names) > 0 {
			list = strings.Join(names, ", ")
		}
		return nil, c.validation(op, keyPath(k), args.vals[k], true, "one of: "+list,
			fmt.Sprintf("Remove %s; %s does not accept it.", k, op.ID))
	}
	return args, nil
}

// runValidator runs a validator; panicked is true when it panicked.
func runValidator(v Validator, value any) (issues []Issue, panicked bool) {
	defer func() {
		if recover() != nil {
			issues, panicked = nil, true
		}
	}()
	return v.Validate(value), false
}

// judge runs a validator that only judges (responses, page items, events):
// a panic is an issue at the root.
func judge(v Validator, value any) []Issue {
	issues, panicked := runValidator(v, value)
	if panicked {
		return []Issue{{Message: "the response schema failed (the validator panicked)"}}
	}
	return issues
}

func (c *ClientCore) checkKey(op *OperationDescriptor, key string, hasKey bool, p purpose) *Error {
	policy := op.Agent.Idempotency
	note := ""
	if policy.Note != "" {
		note = " " + policy.Note
	}
	if !hasKey {
		if p == purposeCall && policy.Policy == IdempotencyCallerOwned && policy.PersistRequired {
			fresh := "unique key"
			if strings.Contains(strings.ToLower(policy.Format), "uuid") {
				fresh = "UUIDv4"
			}
			return newDiag(op.ID, ValidationFailed).
				param("IdempotencyKey").
				expected(keyFormatDescription(policy.Format)).
				remedy(fmt.Sprintf("Generate a random %s, persist it with the intent of this %s call, and pass it as IdempotencyKey. Reuse the exact same key on every retry; a new key can apply the effect twice.%s", fresh, op.ID, note)).
				err()
		}
		return nil
	}
	if policy.Policy == IdempotencyNone {
		keyed := false
		for _, prm := range op.Params {
			if prm.Role == RoleIdempotencyKey {
				keyed = true
			}
		}
		if !keyed {
			return nil
		}
	}
	if policy.Policy == IdempotencyContentIdentity || policy.Policy == IdempotencyContentHash {
		return nil
	}
	format := ""
	if policy.Policy == IdempotencyCallerOwned {
		format = policy.Format
	}
	if expected, ok := checkKeyFormat(key, format); !ok {
		return newDiag(op.ID, ValidationFailed).
			param("IdempotencyKey").
			received(envelopeValue(key, false)).
			expected(expected).
			remedy(fmt.Sprintf("Pass IdempotencyKey as %s. Generate it once, persist it with the intent of this call, and reuse it on every retry.%s", expected, note)).
			err()
	}
	return nil
}

// confirmed applies the confirmation rule of a tier: destructive accepts
// ConfirmYes (unless refused) or a token, irreversible only a token issued
// for subject and these exact args.
func (c *ClientCore) confirmed(t confirmTarget, safety Safety, args any, opts CallOptions) *Error {
	if safety != Destructive && safety != Irreversible {
		return nil
	}
	tier := "irreversible"
	if safety == Destructive {
		tier = "destructive"
	}
	allowYes := !opts.RefuseConfirmYes
	how := fmt.Sprintf("call %s with the same arguments and pass its confirmation_token as confirm", t.previewCall)
	required := func(remediation string) *Error {
		return newDiag(t.id, ConfirmationRequired).
			param("confirm").
			expected("a confirmation_token from preview()").
			remedy(remediation).
			err()
	}
	switch {
	case opts.Confirm.yes:
		if safety == Destructive && allowYes {
			return nil
		}
		return required(fmt.Sprintf("%s is %s, so tungsten.ConfirmYes is not accepted: %s.", t.id, tier, how))
	case opts.Confirm.token != "":
		switch checkToken(c.confirmationKey, opts.Confirm.token, t.subject, args, c.now()) {
		case tokenValid:
			return nil
		case tokenExpired:
			return required(fmt.Sprintf("The confirmation token expired (tokens last %d minutes): %s.", ConfirmationTTLMs/60000, how))
		default:
			return required(fmt.Sprintf("The confirmation token was not issued by this client for %s with exactly these arguments: %s.", t.id, how))
		}
	}
	orYes := ""
	if safety == Destructive && allowYes {
		orYes = " (or pass tungsten.ConfirmYes)"
	}
	return required(fmt.Sprintf("%s is %s: %s%s.", t.id, tier, how, orYes))
}

// claimToken spends a valid token on the call about to be sent: its first
// use binds it to that call's replay protection, and a later use is
// accepted only as a retry under the same protection.
func (c *ClientCore) claimToken(id, token, bind string, hasBind bool, previewCall string) *Error {
	now := c.now()
	c.mu.Lock()
	defer c.mu.Unlock()
	for t, u := range c.usedTokens {
		if u.expiry <= now {
			delete(c.usedTokens, t)
		}
	}
	if previous, ok := c.usedTokens[token]; ok {
		if previous.hasBind && hasBind && previous.bind == bind {
			return nil
		}
		var why string
		if !previous.hasBind {
			why = "This call has no idempotency key, so a repeat can apply the effect twice"
		} else if !hasBind {
			why = "It was used with an idempotency key; only a retry with that same key may reuse it"
		} else {
			why = "It was used with another idempotency key; only a retry with that same key may reuse it"
		}
		return newDiag(id, ConfirmationRequired).
			param("confirm").
			expected("a confirmation_token from preview()").
			remedy(fmt.Sprintf("This confirmation token was already used for one %s call. %s. Check whether that call took effect; to send again, call %s and confirm with its new token.", id, why, previewCall)).
			err()
	}
	expiry := now + ConfirmationTTLMs
	if e, _, ok := tokenExpiry(token); ok {
		expiry = e
	}
	c.usedTokens[token] = usedToken{bind: bind, hasBind: hasBind, expiry: expiry}
	return nil
}

func normalizeArgs(raw any) (*Object, any, bool) {
	converted, err := FromGo(raw)
	if err != nil {
		return nil, raw, false
	}
	switch t := converted.(type) {
	case nil:
		return NewObject(), nil, true
	case *Object:
		return t, t, true
	}
	return nil, converted, false
}

func (c *ClientCore) prepare(ctx context.Context, op *OperationDescriptor, rawArgs any, opts CallOptions, p purpose, urlOverride string, claim *macroClaim) (*prepared, *Error) {
	if op == nil {
		return nil, newDiag("<unknown operation>", ValidationFailed).param("operation").
			expected("a valid operation descriptor").
			remedy("The operation descriptor is invalid: the descriptor is missing. Regenerate the SDK or fix the descriptor; nothing was sent.").err()
	}
	if problem := descriptorProblem(op); problem != "" {
		id := op.ID
		if id == "" {
			id = "<unknown operation>"
		}
		return nil, newDiag(id, ValidationFailed).param("operation").
			expected("a valid operation descriptor").
			remedy(fmt.Sprintf("The operation descriptor is invalid: %s. Regenerate the SDK or fix the descriptor; nothing was sent.", problem)).err()
	}
	c.Register(op)
	supplied, shown, ok := normalizeArgs(rawArgs)
	if !ok {
		names := []string{}
		for i, prm := range argParams(op) {
			if i == 3 {
				break
			}
			names = append(names, prm.Name+": ...")
		}
		return nil, newDiag(op.ID, ValidationFailed).param("args").
			received(envelopeValue(shown, false)).
			expected("an object of named arguments").
			remedy(fmt.Sprintf("Pass the arguments of %s as one object, e.g. {%s}.", op.ID, strings.Join(names, ", "))).err()
	}
	if nestsDeeperThan(supplied, maxArgDepth) {
		return nil, c.validation(op, nil, "<deeply nested value>", true,
			fmt.Sprintf("a JSON value nested at most %d levels", maxArgDepth),
			"Pass arguments without such deep nesting.")
	}
	args, err := c.validateArgs(op, supplied, p == purposeMacroPreview)
	if err != nil {
		return nil, err
	}
	hasKey := opts.IdempotencyKey != ""
	if err := c.checkKey(op, opts.IdempotencyKey, hasKey, p); err != nil {
		return nil, err
	}
	if p == purposeCall && claim == nil {
		if err := c.confirmed(confirmTarget{id: op.ID, subject: op.ID, previewCall: "preview(...)"}, op.Agent.Safety, args, opts); err != nil {
			return nil, err
		}
	}
	method := op.Method
	auth := c.resolveAuth(ctx, op, method)
	if auth.failed != "" {
		return nil, newDiag(op.ID, AuthFailed).remedy(auth.failed).err()
	}
	var secrets secretSet
	for _, s := range auth.secrets {
		secrets.insert(s)
	}
	for _, prm := range op.Params {
		if !prm.Sensitive {
			continue
		}
		switch v := args.vals[prm.Name].(type) {
		case string:
			secrets.insert(v)
		case int64, float64:
			secrets.insert(numberText(v))
		}
	}
	for _, path := range sensitiveRequestPaths(op) {
		var found []string
		valuesAt(args, splitPath(path), &found, 0)
		for _, v := range found {
			secrets.insert(v)
		}
	}
	fullURL, displayURL, err := c.buildURL(op, args, &auth.plan, urlOverride)
	if err != nil {
		return nil, err
	}
	var headers headerBag
	c.buildHeaders(op, args, opts, &auth.plan, &headers)

	paramNames := map[string]bool{}
	for _, prm := range argParams(op) {
		paramNames[prm.Name] = true
	}
	bodyPaths := sensitiveBodyPaths(op)
	var value any
	hasValue := false
	if !isSafeMethod(method) || method == "OPTIONS" {
		value, hasValue = bodyValue(op, args, paramNames)
	}
	redact := func(display any) any {
		for _, bp := range bodyPaths {
			if bp == "" {
				return Redacted
			}
		}
		return redactSensitiveKeys(redactPaths(display, bodyPaths), 0)
	}
	encoded, serr := encodeBody(op.Body, value, hasValue, redact)
	if serr != nil {
		var path []PathSegment
		if op.Body != nil && op.Body.Arg != "" && serr.parameter == "body" {
			path = keyPath(op.Body.Arg)
		}
		return nil, c.validation(op, path, serr.value, true, serr.expected,
			fmt.Sprintf("Pass %s as %s.", serr.parameter, serr.expected))
	}
	if encoded.contentType != "" {
		headers.set("Content-Type", encoded.contentType, false)
	}
	header := keyHeader(op)
	key, hasIdem, err := c.idempotencyKey(op, args, opts, p, encoded)
	if err != nil {
		return nil, err
	}
	if hasIdem {
		headers.set(header, key, true)
		secrets.insert(key)
	} else if p != purposeCall && (op.Agent.Idempotency.Policy == IdempotencyAuto || op.Agent.Idempotency.Policy == IdempotencyCallerOwned) {
		headers.set(header, "<set at call time>", false)
	}
	for _, h := range auth.plan.headers {
		headers.set(h.name, h.value, h.secret)
	}
	if p == purposeServerPreview && op.Agent.Preview.Mode == "header" {
		headers.set(op.Agent.Preview.Header, op.Agent.Preview.Value, false)
	}
	for _, e := range headers.entries {
		if validHeaderName(e.name) && validHeaderValue(e.value) {
			continue
		}
		for _, h := range auth.plan.headers {
			if strings.EqualFold(h.name, e.name) {
				return nil, newDiag(op.ID, AuthFailed).
					remedy(fmt.Sprintf("The credential for header %s contains characters not allowed in an HTTP header; check ClientOptions.auth.", e.name)).err()
			}
		}
		var path []PathSegment
		for _, prm := range op.Params {
			if strings.EqualFold(prm.Wire, e.name) {
				path = keyPath(prm.Name)
				break
			}
		}
		shown := any(e.value)
		if e.secret {
			shown = Redacted
		}
		return nil, c.validation(op, path, shown, true,
			"a header value of visible ASCII characters (no line breaks)",
			fmt.Sprintf("Header %s cannot carry this value; remove line breaks and non-Latin-1 characters.", e.name))
	}
	if p == purposeCall {
		if claim != nil {
			if err := claim.claim(c); err != nil {
				return nil, err
			}
		} else if opts.Confirm.token != "" && (op.Agent.Safety == Destructive || op.Agent.Safety == Irreversible) {
			bind, hasBind := key, hasIdem
			if !hasBind && op.Agent.Idempotency.Policy == IdempotencyContentIdentity {
				bind, hasBind = "content-identity", true
			}
			if err := c.claimToken(op.ID, opts.Confirm.token, bind, hasBind, "preview(...)"); err != nil {
				return nil, err
			}
		}
	}
	for _, s := range headers.secrets() {
		secrets.insert(s)
	}
	var hidden []string
	for _, q := range auth.plan.query {
		hidden = append(hidden, q.k)
	}
	for _, prm := range op.Params {
		if prm.In == InQuery && prm.Sensitive {
			hidden = append(hidden, prm.Wire)
		}
	}
	return &prepared{
		op: op, args: args, method: method, url: fullURL, displayURL: displayURL,
		headers: headers, payload: encoded.payload, displayBody: encoded.display,
		key: key, hasKey: hasIdem, keyHeader: header,
		authQuery: auth.plan.query, hiddenQuery: hidden, secrets: secrets, oauth: auth.oauth,
	}, nil
}

func (c *ClientCore) badBase(op *OperationDescriptor) *Error {
	return newDiag(op.ID, TransportFailed).retry(RetryNever).
		remedy("The base URL is not a valid absolute http(s) URL without credentials: fix ClientOptions.BaseURL. Nothing was sent.").err()
}

func (c *ClientCore) buildURL(op *OperationDescriptor, args *Object, plan *authPlan, urlOverride string) (string, string, *Error) {
	if c.baseURL == "" && urlOverride == "" {
		return "", "", newDiag(op.ID, TransportFailed).retry(RetryNever).
			remedy("No base URL is configured: set ClientOptions.BaseURL. Nothing was sent.").err()
	}
	params := argParams(op)
	var full, display string
	if urlOverride != "" {
		full, display = urlOverride, urlOverride
	} else {
		var unknown string
		hasUnknown := false
		var missing *ParamDescriptor
		var path strings.Builder
		rest := op.Path
		for {
			open := strings.IndexByte(rest, '{')
			if open < 0 {
				break
			}
			closeRel := strings.IndexByte(rest[open:], '}')
			if closeRel < 0 {
				break
			}
			end := open + closeRel
			inner := rest[open+1 : end]
			if inner == "" || strings.Contains(inner, "{") {
				path.WriteString(rest[:open+1])
				rest = rest[open+1:]
				continue
			}
			path.WriteString(rest[:open])
			var p *ParamDescriptor
			for _, x := range params {
				if x.In == InPath && x.Wire == inner {
					p = x
					break
				}
			}
			if p == nil {
				for _, x := range params {
					if x.In == InPath && x.Name == inner {
						p = x
						break
					}
				}
			}
			if p == nil {
				unknown, hasUnknown = inner, true
				path.WriteString(rest[open : end+1])
			} else if v, ok := args.Get(p.Name); !ok || v == nil {
				missing = p
				path.WriteString(rest[open : end+1])
			} else {
				path.WriteString(serializePathParam(p, v))
			}
			rest = rest[end+1:]
		}
		path.WriteString(rest)
		built := path.String()
		if hasUnknown {
			return "", "", newDiag(op.ID, ValidationFailed).param("operation").
				expected("a valid operation descriptor").
				remedy(fmt.Sprintf("The path template has a placeholder {%s} with no path parameter; regenerate the SDK.", unknown)).err()
		}
		if missing != nil {
			return "", "", c.validation(op, keyPath(missing.Name), nil, false,
				"a value for path parameter "+missing.Wire, fmt.Sprintf("Pass %s.", missing.Name))
		}
		template := strings.Split(op.Path, "/")
		segments := strings.Split(built, "/")
		if len(template) == len(segments) {
			for i, segment := range segments {
				tmpl := template[i]
				if !strings.Contains(tmpl, "{") || !isDotSegment(segment) {
					continue
				}
				wire := ""
				if _, after, ok := strings.Cut(tmpl, "{"); ok {
					wire, _, _ = strings.Cut(after, "}")
				}
				var p *ParamDescriptor
				for _, x := range params {
					if x.In == InPath && (x.Wire == wire || x.Name == wire) {
						p = x
						break
					}
				}
				var shownValue any = segment
				var at []PathSegment
				name := "the path parameter"
				if p != nil {
					shownValue = args.vals[p.Name]
					at = keyPath(p.Name)
					name = p.Name
				}
				return "", "", c.validation(op, at, shownValue, true,
					"a non-empty path segment other than . and ..",
					fmt.Sprintf("Pass %s as the identifier of one resource; \"%s\" would change the request path.", name, segment))
			}
		}
		var parts, shown []string
		for _, p := range params {
			if p.In != InQuery {
				continue
			}
			v, ok := args.Get(p.Name)
			if !ok {
				continue
			}
			serialized := serializeQueryParam(p, v)
			for _, s := range serialized {
				if p.Sensitive {
					name, _, _ := strings.Cut(s, "=")
					shown = append(shown, name+"="+Redacted)
				} else {
					shown = append(shown, s)
				}
			}
			parts = append(parts, serialized...)
		}
		for _, q := range plan.query {
			parts = append(parts, encodeComponent(q.k)+"="+encodeComponent(q.v))
			shown = append(shown, encodeComponent(q.k)+"="+Redacted)
		}
		root := strings.TrimRight(c.baseURL, "/")
		join := func(list []string) string {
			if len(list) == 0 {
				return ""
			}
			sep := "?"
			if strings.Contains(built, "?") {
				sep = "&"
			}
			return sep + strings.Join(list, "&")
		}
		full = root + built + join(parts)
		display = root + built + join(shown)
	}
	if urlOverride != "" && len(plan.query) > 0 {
		target, err := url.Parse(full)
		if err != nil {
			return "", "", c.badBase(op)
		}
		for _, q := range plan.query {
			if !hasQueryParam(target, q.k) {
				setQueryParam(target, q.k, q.v)
			}
		}
		full = target.String()
		shownURL, err := url.Parse(display)
		if err != nil {
			return "", "", c.badBase(op)
		}
		for _, q := range plan.query {
			setQueryParam(shownURL, q.k, Redacted)
		}
		display = shownURL.String()
	}
	if !validBaseURL(full) {
		return "", "", c.badBase(op)
	}
	return full, display, nil
}

func (c *ClientCore) buildHeaders(op *OperationDescriptor, args *Object, opts CallOptions, plan *authPlan, headers *headerBag) {
	var accept []string
	for _, r := range op.Responses {
		if r.Kind == KindSuccess && r.MediaType != "" && !containsString(accept, r.MediaType) {
			accept = append(accept, r.MediaType)
		}
	}
	if len(accept) > 0 {
		headers.set("Accept", strings.Join(accept, ", "), false)
	}
	apiName := c.api.Name
	if apiName == "" {
		apiName = "api"
	}
	apiVersion := c.api.Version
	if apiVersion == "" {
		apiVersion = "0"
	}
	headers.set("X-Tungsten-Runtime", fmt.Sprintf("tungsten-go/%s %s-sdk/%s", Version, apiName, apiVersion), false)
	headers.set("X-Tungsten-Operation", op.ID, false)
	tungsten := c.api.TungstenVersion
	if tungsten == "" {
		tungsten = Version
	}
	headers.set("User-Agent", fmt.Sprintf("%s-sdk/%s tungsten/%s (go)", apiName, apiVersion, tungsten), false)
	for _, p := range op.Params {
		if p.Role == RoleConstant && p.In == InHeader && p.Constant != nil {
			headers.set(p.Wire, *p.Constant, p.Sensitive)
		}
	}
	for _, extra := range []map[string]string{c.headers, opts.Headers} {
		names := make([]string, 0, len(extra))
		for n := range extra {
			names = append(names, n)
		}
		sort.Strings(names)
		for _, n := range names {
			headers.set(n, extra[n], looksSensitive(n))
		}
	}
	var cookies []string
	if existing, ok := headers.get("Cookie"); ok {
		cookies = append(cookies, existing)
	}
	for _, p := range argParams(op) {
		v, ok := args.Get(p.Name)
		if !ok || v == nil {
			continue
		}
		switch p.In {
		case InHeader:
			headers.set(p.Wire, serializeHeaderParam(p, v), p.Sensitive)
		case InCookie:
			cookies = append(cookies, serializeCookieParam(p, v))
		}
	}
	for _, ck := range plan.cookies {
		cookies = append(cookies, ck.k+"="+ck.v)
	}
	// Cookies are session material: the whole header is always redacted.
	if len(cookies) > 0 {
		headers.set("Cookie", strings.Join(cookies, "; "), true)
	}
}

func (c *ClientCore) idempotencyKey(op *OperationDescriptor, args *Object, opts CallOptions, p purpose, encoded encodedBody) (string, bool, *Error) {
	supplied, hasSupplied := opts.IdempotencyKey, opts.IdempotencyKey != ""
	switch op.Agent.Idempotency.Policy {
	case IdempotencyContentIdentity:
		return "", false, nil
	case IdempotencyContentHash:
		if !encoded.hasHash {
			return "", false, nil
		}
		return sha256Hex(encoded.hashed), true, nil
	case IdempotencyCallerOwned:
		return supplied, hasSupplied, nil
	case IdempotencyAuto:
		if hasSupplied {
			return supplied, true, nil
		}
		if p != purposeCall {
			return "", false, nil
		}
		refuse := func(why string) *Error {
			return newDiag(op.ID, TransportFailed).retry(RetryNever).
				remedy(fmt.Sprintf("The idempotency store failed (%s), so the call was not sent rather than risk a second key for the same intent. Fix or replace ClientOptions.IdempotencyStore.", why)).err()
		}
		logical := sha256Hex([]byte(CanonicalJSON(args)))
		c.keyMu.Lock()
		defer c.keyMu.Unlock()
		if existing, ok := c.store.Get(op.ID, logical); ok && existing != "" {
			return existing, true, nil
		}
		key, ok := uuidV4()
		if !ok {
			return "", false, refuse("no source of randomness")
		}
		c.store.Put(op.ID, logical, key)
		if stored, ok := c.store.Get(op.ID, logical); !ok || stored != key {
			return "", false, refuse("the key could not be stored")
		}
		return key, true, nil
	}
	for _, prm := range op.Params {
		if prm.Role == RoleIdempotencyKey {
			return supplied, hasSupplied, nil
		}
	}
	return "", false, nil
}
