// SPDX-License-Identifier: Apache-2.0

package main

import (
	"encoding/json"
	"fmt"
	"sort"
	"strconv"
	"strings"
)

var categories = map[string]string{
	"VALIDATION_FAILED": "ValidationFailed", "MALFORMED_REQUEST": "MalformedRequest",
	"REQUEST_TOO_LARGE": "RequestTooLarge", "AUTH_FAILED": "AuthFailed", "NOT_FOUND": "NotFound",
	"CONFLICT": "Conflict", "PRECONDITION_FAILED": "PreconditionFailed", "RATE_LIMITED": "RateLimited",
	"UPSTREAM_UNAVAILABLE": "UpstreamUnavailable", "OUTCOME_UNKNOWN": "OutcomeUnknown",
	"TRANSPORT_FAILED": "TransportFailed", "CONFIRMATION_REQUIRED": "ConfirmationRequired",
	"GATE_DISABLED": "GateDisabled", "UNEXPECTED_RESPONSE": "UnexpectedResponse",
}

var retryables = map[string]string{
	"never": "RetryNever", "after_delay": "RetryAfterDelay", "same_key_only": "RetrySameKeyOnly",
	"after_remediation": "RetryAfterRemediation",
}

var safeties = map[string]string{
	"read_only": "ReadOnly", "mutating": "Mutating", "destructive": "Destructive", "irreversible": "Irreversible",
}

var policies = map[string]string{
	"none": "IdempotencyNone", "auto": "IdempotencyAuto", "caller_owned": "IdempotencyCallerOwned",
	"content_hash": "IdempotencyContentHash", "content_identity": "IdempotencyContentIdentity",
}

var styles = map[string]string{
	"simple": "StyleSimple", "form": "StyleForm", "label": "StyleLabel", "matrix": "StyleMatrix",
	"space_delimited": "StyleSpaceDelimited", "pipe_delimited": "StylePipeDelimited", "deep_object": "StyleDeepObject",
}

var roles = map[string]string{
	"plain": "RolePlain", "idempotency_key": "RoleIdempotencyKey", "dry_run": "RoleDryRun",
	"origin": "RoleOrigin", "auth": "RoleAuth", "constant": "RoleConstant",
}

var locations = map[string]string{"path": "InPath", "query": "InQuery", "header": "InHeader", "cookie": "InCookie"}

var encodings = map[string]string{
	"json": "EncodeJSON", "form": "EncodeForm", "multipart": "EncodeMultipart", "bytes": "EncodeBytes",
	"text": "EncodeText", "jsonl": "EncodeBytes",
}

func rt(table map[string]string, key, fallback string) string {
	if v, ok := table[key]; ok {
		return "tungsten." + v
	}
	return "tungsten." + fallback
}

func stringList(list []string) string {
	quoted := make([]string, len(list))
	for i, s := range list {
		quoted[i] = goString(s)
	}
	return "[]string{" + strings.Join(quoted, ", ") + "}"
}

func isNull(raw json.RawMessage) bool {
	t := strings.TrimSpace(string(raw))
	return t == "" || t == "null"
}

func millis(n *int64) string {
	if n == nil {
		return ""
	}
	return fmt.Sprintf("tungsten.Millis(%d)", *n)
}

func remediationLit(r Remediation) string {
	var parts []string
	if c, ok := categories[r.Category]; ok {
		parts = append(parts, "Category: tungsten."+c)
	}
	if r.Text != "" {
		parts = append(parts, "Text: "+goString(r.Text))
	}
	if v, ok := retryables[r.Retryable]; ok {
		parts = append(parts, "Retryable: tungsten."+v)
	}
	if r.NextAction != "" {
		parts = append(parts, "NextAction: "+goString(r.NextAction))
	}
	return "{" + strings.Join(parts, ", ") + "}"
}

func remediationMap(m map[string]Remediation) string {
	if len(m) == 0 {
		return ""
	}
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	var b strings.Builder
	b.WriteString("map[string]tungsten.RemediationEntry{\n")
	for _, k := range keys {
		fmt.Fprintf(&b, "%s: %s,\n", goString(k), remediationLit(m[k]))
	}
	b.WriteString("}")
	return b.String()
}

// constantText is the header text of a constant parameter.
func constantText(p *Param) (string, bool) {
	if p.Role != "constant" || isNull(p.Constant) {
		return "", false
	}
	var s string
	if json.Unmarshal(p.Constant, &s) == nil {
		return s, true
	}
	return compactJSON(p.Constant), true
}

// opSummary is the pruned agent doc, else the spec summary, on one line.
func opSummary(op *Operation) string {
	text := op.Agent.CompactDoc
	if strings.TrimSpace(text) == "" {
		text = docSummary(op.Doc)
	}
	return strings.Join(strings.Fields(text), " ")
}

func docSummary(d *Doc) string {
	if d == nil {
		return ""
	}
	if s := strings.TrimSpace(d.Summary); s != "" {
		return s
	}
	desc := strings.TrimSpace(d.Description)
	first, _, _ := strings.Cut(desc, "\n")
	return first
}

func (g *gen) codeField(ns *Namespace) string {
	return ns.Errors.CodeField
}

// paramLit is one ParamDescriptor literal.
func paramLit(a argParam) string {
	p := a.param
	parts := []string{
		"Name: " + goString(a.key),
		"Wire: " + goString(p.WireName),
		"In: " + rt(locations, a.location, "InQuery"),
	}
	if p.Required {
		parts = append(parts, "Required: true")
	}
	parts = append(parts, "Style: "+rt(styles, p.Style, "StyleSimple"))
	if p.Explode {
		parts = append(parts, "Explode: true")
	}
	parts = append(parts, "Role: "+rt(roles, p.Role, "RolePlain"))
	if c, ok := constantText(p); ok {
		parts = append(parts, "Constant: tungsten.Ptr("+goString(c)+")")
	}
	return "{" + strings.Join(parts, ", ") + "}"
}

// requestSchema is the schema of the arguments object.
func (g *gen) requestSchema(p *opPlan) string {
	hint := splitWords(p.request)
	var fields []string
	for _, a := range p.params {
		fields = append(fields, g.fieldSchema(a.key, g.schemaRef(&a.param.Ty, append(append([]string(nil), hint...), a.param.Name.Words...)), Constraints{}, a.param.Required, false))
	}
	if b := p.body; b != nil {
		if b.merged != nil {
			for _, f := range b.merged {
				fields = append(fields, g.fieldSchema(f.WireName, g.schemaRef(&f.Ty, append(append([]string(nil), hint...), f.Name.Words...)), fieldConstraints(f), f.Required(), f.Nullable()))
			}
		} else {
			var schema string
			switch b.content.Encoding {
			case "bytes", "jsonl":
				schema = schemaLit([]string{`Kind: "binary"`})
			case "text":
				schema = schemaLit([]string{`Kind: "string"`})
			default:
				schema = g.schemaRef(&b.content.Ty, append(append([]string(nil), hint...), "body"))
			}
			fields = append(fields, g.fieldSchema(b.arg, schema, Constraints{}, b.required, false))
		}
	}
	parts := []string{`Kind: "object"`}
	if len(fields) > 0 {
		parts = append(parts, "Fields: []tungsten.SchemaField{\n"+strings.Join(fields, ",\n")+",\n}")
	}
	if p.op.RPC != nil && p.body == nil {
		parts = append(parts, `Additional: "open"`)
	} else {
		parts = append(parts, `Additional: "closed"`)
	}
	return schemaLit(parts)
}

func (g *gen) paginationLit(p *opPlan) string {
	pg := p.op.Pagination
	if pg == nil {
		return ""
	}
	arg := func(wire string) string {
		for _, a := range p.params {
			if a.param.WireName == wire && a.location == "query" {
				return a.key
			}
		}
		return wire
	}
	parts := []string{"ItemsField: " + goString(pg.ItemsField)}
	switch pg.Style.Kind {
	case "cursor":
		parts = append([]string{`Style: "cursor"`, "RequestParam: " + goString(arg(pg.Style.RequestParam)), "ResponseField: " + goString(pg.Style.ResponseField)}, parts...)
		if pg.PageSizeParam != nil {
			parts = append(parts, "PageSizeParam: "+goString(arg(*pg.PageSizeParam)))
		}
		if pg.HasMoreField != nil {
			parts = append(parts, "HasMoreField: "+goString(*pg.HasMoreField))
		}
		if pg.CursorItemField != nil {
			parts = append(parts, "CursorItemField: "+goString(*pg.CursorItemField))
		}
	case "offset":
		parts = append([]string{`Style: "offset"`, "OffsetParam: " + goString(arg(pg.Style.OffsetParam)), "LimitParam: " + goString(arg(pg.Style.LimitParam))}, parts...)
	case "page":
		parts = append([]string{`Style: "page"`, "PageParam: " + goString(arg(pg.Style.PageParam)), "SizeParam: " + goString(arg(pg.Style.SizeParam))}, parts...)
	default:
		parts = append([]string{`Style: "link_header"`}, parts...)
	}
	return "&tungsten.Pagination{" + strings.Join(parts, ", ") + "}"
}

func (g *gen) agentLit(p *opPlan) string {
	a := &p.op.Agent
	var parts []string
	parts = append(parts, "Safety: "+rt(safeties, a.Safety, "Mutating"))
	idem := []string{"Policy: " + rt(policies, a.Idempotency.Policy, "IdempotencyNone")}
	if a.Idempotency.Header != "" {
		idem = append(idem, "Header: "+goString(a.Idempotency.Header))
	}
	if a.Idempotency.Format != "" {
		idem = append(idem, "Format: "+goString(a.Idempotency.Format))
	}
	if a.Idempotency.PersistRequired {
		idem = append(idem, "PersistRequired: true")
	}
	if a.Idempotency.Note != "" {
		idem = append(idem, "Note: "+goString(a.Idempotency.Note))
	}
	parts = append(parts, "Idempotency: tungsten.IdempotencyMeta{"+strings.Join(idem, ", ")+"}")
	preview := []string{"Mode: " + goString(orDefault(a.Preview.Mode, "local"))}
	switch a.Preview.Mode {
	case "header":
		preview = append(preview, "Header: "+goString(a.Preview.Header), "Value: "+goString(a.Preview.Value))
	case "endpoint":
		preview = append(preview, "Operation: "+goString(a.Preview.Operation))
	}
	parts = append(parts, "Preview: tungsten.PreviewMode{"+strings.Join(preview, ", ")+"}")
	if c := a.Confirmation; c != nil {
		conf := []string{"SummaryFields: " + stringList(c.SummaryFields)}
		if c.Message != "" {
			conf = append(conf, "Message: "+goString(c.Message))
		}
		parts = append(parts, "Confirmation: &tungsten.ConfirmationMeta{"+strings.Join(conf, ", ")+"}")
	}
	if v := a.Verify; v != nil {
		verify := []string{"Operation: " + goString(v.Operation)}
		if !isNull(v.Args) {
			verify = append(verify, "Args: "+jsonValueLit(v.Args))
		}
		if !isNull(v.Expect) {
			verify = append(verify, "Expect: "+jsonValueLit(v.Expect))
		}
		if !isNull(v.Terminal) {
			verify = append(verify, "Terminal: "+jsonValueLit(v.Terminal))
		}
		if m := millis(v.PollIntervalMs); m != "" {
			verify = append(verify, "PollInterval: "+m)
		}
		if m := millis(v.PollBudgetMs); m != "" {
			verify = append(verify, "PollBudget: "+m)
		}
		parts = append(parts, "Verify: &tungsten.VerifyDescriptor{"+strings.Join(verify, ", ")+"}")
	}
	if m := remediationMap(a.Remediation); m != "" {
		parts = append(parts, "Remediation: "+m)
	}
	if a.RemediationNote != "" {
		parts = append(parts, "RemediationNote: "+goString(a.RemediationNote))
	}
	if len(a.SensitiveResponseFields) > 0 {
		parts = append(parts, "SensitiveResponseFields: "+stringList(a.SensitiveResponseFields))
	}
	if s := g.sensitiveRequestFields(p); len(s) > 0 {
		parts = append(parts, "SensitiveRequestFields: "+stringList(s))
	}
	if a.ShownOnce {
		parts = append(parts, "ShownOnce: true")
	}
	return "tungsten.AgentMeta{\n" + strings.Join(parts, ",\n") + ",\n}"
}

func orDefault(s, d string) string {
	if s == "" {
		return d
	}
	return s
}

func statusLit(s StatusMatch) (string, bool) {
	switch s.Kind {
	case "exact":
		return fmt.Sprintf("tungsten.Exact(%d)", s.Value), true
	case "range":
		if s.Value >= 1 && s.Value <= 5 {
			return fmt.Sprintf("tungsten.Class(%d)", s.Value), true
		}
		return "", false
	case "default":
		return "tungsten.DefaultStatus", true
	}
	return "", false
}

var responseKinds = map[string]string{"success": "KindSuccess", "error": "KindError", "ambiguous": "KindAmbiguous"}

// descriptorLit is the OperationDescriptor literal of one operation.
func (g *gen) descriptorLit(p *opPlan) string {
	op := p.op
	var b strings.Builder
	line := func(format string, args ...any) {
		fmt.Fprintf(&b, format, args...)
		b.WriteByte('\n')
	}
	line("&tungsten.OperationDescriptor{")
	line("ID: %s,", goString(op.ID))
	line("Method: %s,", goString(op.Method))
	line("Path: %s,", goString(op.Path.Raw))
	all := append(append([]argParam(nil), p.params...), p.supplied...)
	if len(all) > 0 {
		line("Params: []tungsten.ParamDescriptor{")
		for _, a := range all {
			line("%s,", paramLit(a))
		}
		line("},")
	}
	if body := p.body; body != nil {
		parts := []string{
			"MediaType: " + goString(body.content.MediaType),
			"Encoding: " + rt(encodings, body.content.Encoding, "EncodeJSON"),
		}
		if body.required {
			parts = append(parts, "Required: true")
		}
		if body.merged != nil {
			fields := make([]string, len(body.merged))
			for i, f := range body.merged {
				fields[i] = fmt.Sprintf("{Arg: %s, Wire: %s}", goString(f.WireName), goString(f.WireName))
			}
			parts = append(parts, "Fields: []tungsten.MergedField{"+strings.Join(fields, ", ")+"}")
		} else {
			parts = append(parts, "Arg: "+goString(body.arg))
		}
		line("Body: &tungsten.BodyDescriptor{%s},", strings.Join(parts, ", "))
	}
	var responses []string
	for i := range op.Responses {
		r := &op.Responses[i]
		status, ok := statusLit(r.Status)
		if !ok {
			continue
		}
		parts := []string{"Status: " + status, "Kind: " + rt(responseKinds, r.Kind.Kind, "KindError")}
		if len(r.Content) > 0 {
			parts = append(parts, "MediaType: "+goString(r.Content[0].MediaType))
		}
		responses = append(responses, "{"+strings.Join(parts, ", ")+"}")
	}
	if len(responses) > 0 {
		line("Responses: []tungsten.ResponseDescriptor{")
		for _, r := range responses {
			line("%s,", r)
		}
		line("},")
	}
	if len(op.Security) > 0 {
		var alts []string
		for _, req := range op.Security {
			var names []string
			for _, s := range req.AllOf {
				names = append(names, s.Scheme)
			}
			alts = append(alts, strings.TrimPrefix(stringList(names), "[]string"))
		}
		line("Security: [][]string{%s},", strings.Join(alts, ", "))
	}
	if pg := g.paginationLit(p); pg != "" {
		line("Pagination: %s,", pg)
	}
	if r := op.RPC; r != nil {
		parts := []string{
			"Field: " + goString(r.DiscriminatorField),
			"Value: " + goString(r.DiscriminatorValue),
			"ParamsField: " + goString(r.ParamsField),
		}
		if len(r.Constants) > 0 {
			keys := make([]string, 0, len(r.Constants))
			for k := range r.Constants {
				keys = append(keys, k)
			}
			sort.Strings(keys)
			var members []string
			for _, k := range keys {
				members = append(members, strconv.Quote(k)+":"+compactJSON(r.Constants[k]))
			}
			parts = append(parts, "Constants: tungsten.MustJSONObject("+goString("{"+strings.Join(members, ",")+"}")+")")
		}
		line("RPC: &tungsten.RPCDescriptor{%s},", strings.Join(parts, ", "))
	}
	if f := g.codeField(p.ns); f != "" {
		line("ErrorCodeField: %s,", goString(f))
	}
	if op.Status.Kind == "gated" && op.Status.Gate != nil {
		line("Gate: &tungsten.Gate{EnvVar: %s, DisabledStatus: %d},", goString(op.Status.Gate.EnvVar), op.Status.Gate.DisabledStatus)
	}
	line("Agent: %s,", g.agentLit(p))
	line("Request: %s,", g.requestSchema(p))
	if p.response != "" {
		line("Response: %s,", p.response)
	}
	if p.itemSchema != "" {
		line("PageItem: %s,", p.itemSchema)
	}
	if s := opSummary(op); s != "" {
		line("Summary: %s,", goString(s))
	}
	b.WriteString("}")
	return b.String()
}

func retryLit(r RetryPolicy, honor bool) string {
	return fmt.Sprintf("tungsten.PartialRetry{Max: tungsten.Ptr(%d), Base: tungsten.Millis(%d), MaxDelay: tungsten.Millis(%d), Jitter: %s, HonorRetryAfter: tungsten.Ptr(%t)}",
		r.Max, r.BaseMs, r.MaxMs, rt(map[string]string{"none": "JitterNone", "full": "JitterFull", "equal": "JitterEqual"}, r.Jitter, "JitterFull"), honor)
}

func (g *gen) authLits() []string {
	var out []string
	for _, s := range g.ir.Auth {
		switch s.Kind {
		case "api_key":
			out = append(out, fmt.Sprintf("{Kind: \"api_key\", Name: %s, In: %s, Wire: %s}", goString(s.Name), rt(locations, s.Location, "InHeader"), goString(s.WireName)))
		case "http_bearer", "open_id_connect":
			if s.Kind == "open_id_connect" {
				g.diags = append(g.diags, diagnostic{Code: "GO002", Severity: "info",
					Message: "OpenID Connect scheme `" + s.Name + "` is sent as a bearer token; the Go SDK does not run discovery or obtain tokens"})
			}
			lit := fmt.Sprintf("{Kind: \"http_bearer\", Name: %s", goString(s.Name))
			if s.Kind == "http_bearer" && s.Prefix != "" {
				lit += ", Prefix: " + goString(s.Prefix)
			}
			out = append(out, lit+"}")
		case "http_basic":
			out = append(out, fmt.Sprintf("{Kind: \"http_basic\", Name: %s}", goString(s.Name)))
		case "o_auth2":
			tokenURL := ""
			for _, f := range s.Flows {
				if f.Kind == "clientCredentials" && f.TokenURL != "" {
					tokenURL = f.TokenURL
					break
				}
			}
			if tokenURL == "" {
				for _, f := range s.Flows {
					if f.TokenURL != "" {
						tokenURL = f.TokenURL
						break
					}
				}
			}
			scopeSet := map[string]bool{}
			for _, f := range s.Flows {
				for k := range f.Scopes {
					scopeSet[k] = true
				}
			}
			scopes := make([]string, 0, len(scopeSet))
			for k := range scopeSet {
				scopes = append(scopes, k)
			}
			sort.Strings(scopes)
			parts := []string{`Kind: "oauth2"`, "Name: " + goString(s.Name)}
			if tokenURL != "" {
				parts = append(parts, "TokenURL: "+goString(tokenURL))
			}
			if len(scopes) > 0 {
				parts = append(parts, "Scopes: "+stringList(scopes))
			}
			for _, f := range s.Flows {
				if f.Kind == "authorizationCode" && f.AuthorizationURL != "" && f.TokenURL != "" {
					code := []string{"AuthorizationURL: " + goString(f.AuthorizationURL), "TokenURL: " + goString(f.TokenURL)}
					if f.RefreshURL != "" {
						code = append(code, "RefreshURL: "+goString(f.RefreshURL))
					}
					code = append(code, "Scopes: "+stringList(f.ScopeNames()))
					parts = append(parts, "AuthorizationCode: &tungsten.AuthorizationCodeFlow{"+strings.Join(code, ", ")+"}")
					break
				}
			}
			out = append(out, "{"+strings.Join(parts, ", ")+"}")
		case "composite":
			var parts []string
			for _, part := range s.Parts {
				fields := []string{"Kind: " + goString(part.Kind)}
				if part.Name != "" {
					fields = append(fields, "Name: "+goString(part.Name))
				}
				if part.EqualsCookie != "" {
					fields = append(fields, "EqualsCookie: "+goString(part.EqualsCookie))
				}
				if part.FromConfig != "" {
					fields = append(fields, "FromConfig: "+goString(part.FromConfig))
				}
				if part.MutationOnly {
					fields = append(fields, "MutationOnly: true")
				}
				if part.Prefix != "" {
					fields = append(fields, "Prefix: "+goString(part.Prefix))
				}
				parts = append(parts, "{"+strings.Join(fields, ", ")+"}")
			}
			out = append(out, fmt.Sprintf("{Kind: \"composite\", Name: %s, Satisfies: %s, Parts: []tungsten.CompositePart{%s}}",
				goString(s.Name), stringList(s.Satisfies), strings.Join(parts, ", ")))
		}
	}
	return out
}

// apiLit is the APIDescriptor literal.
func (g *gen) apiLit() string {
	ir := g.ir
	var b strings.Builder
	line := func(format string, args ...any) {
		fmt.Fprintf(&b, format, args...)
		b.WriteByte('\n')
	}
	line("&tungsten.APIDescriptor{")
	line("Name: %s,", goString(ir.API.Name.Wire))
	line("Version: %s,", goString(g.opts.version))
	line("TungstenVersion: %s,", goString(ir.Generator.TungstenVersion))
	if len(ir.API.Servers) > 0 {
		urls := make([]string, len(ir.API.Servers))
		for i, s := range ir.API.Servers {
			urls[i] = s.URL
		}
		line("Servers: %s,", stringList(urls))
	}
	if auth := g.authLits(); len(auth) > 0 {
		line("Auth: []tungsten.AuthScheme{")
		for _, a := range auth {
			line("%s,", a)
		}
		line("},")
	}
	if m := remediationMap(ir.Agent.ErrorCodes); m != "" {
		line("ErrorCodes: %s,", m)
	}
	if len(ir.Agent.AmbiguousStatuses) > 0 {
		codes := make([]string, len(ir.Agent.AmbiguousStatuses))
		for i, c := range ir.Agent.AmbiguousStatuses {
			codes[i] = strconv.Itoa(c)
		}
		line("AmbiguousStatuses: []int{%s},", strings.Join(codes, ", "))
	}
	var nonJSON []string
	for _, e := range ir.Agent.NonJSON {
		c, ok := categories[e.Category]
		if !ok {
			continue
		}
		parts := []string{fmt.Sprintf("Status: %d", e.Status), "Media: " + goString(e.Media), "Category: tungsten." + c,
			"Retryable: " + rt(retryables, e.Retryable, "RetryNever")}
		if e.Text != "" {
			parts = append(parts, "Text: "+goString(e.Text))
		}
		nonJSON = append(nonJSON, "{"+strings.Join(parts, ", ")+"}")
	}
	if len(nonJSON) > 0 {
		line("NonJSON: []tungsten.NonJSONError{")
		for _, n := range nonJSON {
			line("%s,", n)
		}
		line("},")
	}
	if len(ir.Agent.Gates) > 0 {
		keys := make([]string, 0, len(ir.Agent.Gates))
		for k := range ir.Agent.Gates {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		line("Gates: map[string]string{")
		for _, k := range keys {
			line("%s: %s,", goString(k), goString(ir.Agent.Gates[k]))
		}
		line("},")
	}
	r := ir.Agent.Retries
	line("Retries: &tungsten.TierRetries{")
	line("ReadOnly: %s,", retryLit(r.ReadOnly, r.HonorRetryAfter))
	line("Mutating: %s,", retryLit(r.Mutating, r.HonorRetryAfter))
	line("},")
	b.WriteString("}")
	return b.String()
}
