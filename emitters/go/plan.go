// SPDX-License-Identifier: Apache-2.0

package main

import (
	"sort"
	"strings"
)

// gen holds everything one run plans and writes.
type gen struct {
	ir     *IR
	opts   options
	multi  bool
	names  *scope
	types  []*goType
	byID   map[string]*goType
	inline map[*Shape]*goType
	ops    []*opPlan
	byOp   map[string]*opPlan
	roots  []*resPlan
	diags  []diagnostic
	macros []*macroPlan
	// consts are the constant names of each enum type.
	consts map[*goType][]string
}

// argParam is one parameter of an operation's arguments object.
type argParam struct {
	key      string
	location string
	param    *Param
	goName   string
	goType   string
}

// bodyPlan is how the body travels in the arguments.
type bodyPlan struct {
	content  *BodyContent
	required bool
	// merged fields (arg key = wire name) or the whole-body argument.
	merged []*Field
	arg    string
	goName string
	goType string
	// fieldGo are the Go names and types of merged fields.
	fieldGo []fieldPlan
}

// opPlan is one callable operation.
type opPlan struct {
	op        *Operation
	ns        *Namespace
	res       *resPlan
	params    []argParam // arguments
	supplied  []argParam // supplied by options or the auth profile
	body      *bodyPlan
	descVar   string
	method    string
	request   string
	success   string
	successRT *TypeRef
	bodiless  bool
	// response is the response schema expression, "" for none.
	response    string
	itemType    string
	itemSchema  string
	streamVar   string
	eventType   string
	eventSchema string
}

// resPlan is one resource struct.
type resPlan struct {
	res      *Resource
	ns       *Namespace
	goName   string
	words    []string
	ops      []*opPlan
	children []*resPlan
	accessor string
}

func isSupplied(p *Param) bool {
	switch p.Role {
	case "idempotency_key", "origin", "auth", "constant":
		return true
	}
	return false
}

func isArg(p *Param) bool {
	return p.Role == "plain" || p.Role == "dry_run"
}

// layout is tungsten_emit::args::args_layout: arguments keyed by their
// TypeScript parameter name, merged JSON record bodies keyed by wire name,
// any other body as one argument.
func (g *gen) layout(op *Operation) ([]argParam, []argParam, *bodyPlan) {
	type located struct {
		loc string
		p   *Param
	}
	var all []located
	for _, group := range []struct {
		loc  string
		list []Param
	}{{"path", op.Params.Path}, {"query", op.Params.Query}, {"header", op.Params.Header}, {"cookie", op.Params.Cookie}} {
		for i := range group.list {
			all = append(all, located{group.loc, &group.list[i]})
		}
	}
	var ordered []located
	for _, l := range all {
		if !isSupplied(l.p) {
			ordered = append(ordered, l)
		}
	}
	for _, l := range all {
		if isSupplied(l.p) {
			ordered = append(ordered, l)
		}
	}
	words := make([][]string, len(ordered))
	for i, l := range ordered {
		words[i] = l.p.Name.Words
	}
	keys := disambiguate(words, tsParam)
	var params, supplied []argParam
	for i, l := range ordered {
		a := argParam{key: keys[i], location: l.loc, param: l.p}
		if isSupplied(l.p) {
			supplied = append(supplied, a)
		} else {
			params = append(params, a)
		}
	}
	taken := make([]string, len(params))
	for i, p := range params {
		taken[i] = p.key
	}
	if op.Body == nil || len(op.Body.Content) == 0 {
		return params, supplied, nil
	}
	content := &op.Body.Content[0]
	for i := range op.Body.Content {
		if op.Body.Content[i].Encoding == "json" {
			content = &op.Body.Content[i]
			break
		}
	}
	body := &bodyPlan{content: content, required: op.Body.Required}
	if s := g.ir.Resolve(&content.Ty); content.Encoding == "json" && s != nil && s.Kind == "record" && s.Additional.Kind != "typed" {
		var sent []*Field
		for i := range s.Fields {
			if !s.Fields[i].ReadOnly {
				sent = append(sent, &s.Fields[i])
			}
		}
		allOptional := true
		clash := false
		for _, f := range sent {
			if f.Presence != "optional" && f.Presence != "optional_nullable" {
				allOptional = false
			}
			for _, k := range taken {
				if k == f.WireName {
					clash = true
				}
			}
		}
		if (op.Body.Required || allOptional) && !clash {
			body.merged = sent
			return params, supplied, body
		}
	}
	entries := make([][]string, 0, len(taken)+1)
	for _, k := range taken {
		entries = append(entries, splitWords(k))
	}
	entries = append(entries, []string{"body"})
	rendered := disambiguate(entries, tsParam)
	body.arg = rendered[len(rendered)-1]
	return params, supplied, body
}

// planResources builds the resource tree and names its structs, methods and
// request types.
func (g *gen) planResources() {
	for n := range g.ir.Namespaces {
		ns := &g.ir.Namespaces[n]
		var nsWords []string
		if g.multi {
			nsWords = ns.Name.Words
		}
		rwords := make([][]string, len(ns.Resources))
		for i := range ns.Resources {
			rwords[i] = ns.Resources[i].Name.Words
		}
		accessors := goNames(rwords, clientMembers...)
		if g.multi {
			accessors = goNames(rwords)
		}
		for i := range ns.Resources {
			r := g.planResource(&ns.Resources[i], ns, nsWords)
			r.accessor = accessors[i]
			g.roots = append(g.roots, r)
		}
	}
}

func (g *gen) planResource(r *Resource, ns *Namespace, parent []string) *resPlan {
	words := append(append([]string(nil), parent...), r.Name.Words...)
	rp := &resPlan{res: r, ns: ns, words: words, goName: g.names.alloc(goName(append(append([]string(nil), words...), "resource")))}
	childWords := make([][]string, len(r.Children))
	for i := range r.Children {
		childWords[i] = r.Children[i].Name.Words
	}
	opWords := make([][]string, len(r.Operations))
	for i := range r.Operations {
		opWords[i] = r.Operations[i].Name.Words
	}
	// Children and methods share the struct's method set.
	members := goNames(append(childWords, opWords...))
	for i := range r.Children {
		child := g.planResource(&r.Children[i], ns, words)
		child.accessor = members[i]
		rp.children = append(rp.children, child)
	}
	for i := range r.Operations {
		op := &r.Operations[i]
		p := &opPlan{op: op, ns: ns, res: rp, method: members[len(r.Children)+i]}
		p.request = g.names.alloc(goName(append(append(append([]string(nil), words...), op.Name.Words...), "request")))
		p.descVar = g.names.alloc("Op" + goName(splitWords(op.ID)))
		rp.ops = append(rp.ops, p)
		g.ops = append(g.ops, p)
		g.byOp[op.ID] = p
	}
	return rp
}

// planOps fills each operation's arguments and result types.
func (g *gen) planOps() {
	for _, p := range g.ops {
		op := p.op
		p.params, p.supplied, p.body = g.layout(op)
		hint := splitWords(p.request)
		hint = hint[:len(hint)-1]
		pwords := make([][]string, 0, len(p.params)+1)
		for _, a := range p.params {
			pwords = append(pwords, a.param.Name.Words)
		}
		var fieldNames []string
		var bodyFields []*Field
		if p.body != nil && p.body.merged != nil {
			bodyFields = p.body.merged
			for _, f := range bodyFields {
				pwords = append(pwords, f.Name.Words)
			}
		} else if p.body != nil {
			pwords = append(pwords, []string{"body"})
		}
		fieldNames = goNames(pwords)
		for i := range p.params {
			a := &p.params[i]
			a.goName = fieldNames[i]
			base := g.goTypeOf(&a.param.Ty, append(append([]string(nil), hint...), a.param.Name.Words...))
			if a.param.Required {
				a.goType = base
			} else if pointerLike(base) {
				a.goType = base
			} else {
				a.goType = "*" + base
			}
		}
		if p.body != nil {
			if bodyFields != nil {
				for i, f := range bodyFields {
					base := g.goTypeOf(&f.Ty, append(append([]string(nil), hint...), f.Name.Words...))
					plan := fieldPlan{goName: fieldNames[len(p.params)+i], field: f}
					switch f.Presence {
					case "required":
						plan.goType, plan.tag = base, f.WireName
					case "required_nullable":
						plan.goType, plan.tag = base, f.WireName
						if !pointerLike(base) {
							plan.goType = "*" + base
						}
					case "optional_nullable":
						plan.goType, plan.tag, plan.patch = "*tungsten.Patch["+base+"]", f.WireName+",omitempty", true
					default:
						plan.goType, plan.tag = base, f.WireName+",omitempty"
						if base != "json.RawMessage" && !strings.HasPrefix(base, "*") {
							plan.goType = "*" + base
						}
					}
					p.body.fieldGo = append(p.body.fieldGo, plan)
				}
			} else {
				p.body.goName = fieldNames[len(p.params)]
				var base string
				switch p.body.content.Encoding {
				case "bytes", "jsonl":
					base = "tungsten.Binary"
				case "text":
					base = "string"
				default:
					base = g.goTypeOf(&p.body.content.Ty, append(append([]string(nil), hint...), "body"))
				}
				if p.body.required || pointerLike(base) {
					p.body.goType = base
				} else {
					p.body.goType = "*" + base
				}
			}
		}
		g.planSuccess(p, hint)
	}
}

func firstContent(r *Response) *BodyContent {
	for i := range r.Content {
		if r.Content[i].Encoding == "json" {
			return &r.Content[i]
		}
	}
	if len(r.Content) > 0 {
		return &r.Content[0]
	}
	return nil
}

// planSuccess decides the typed result, the response schema and the page
// item and event types.
func (g *gen) planSuccess(p *opPlan, hint []string) {
	op := p.op
	type typed struct {
		goType string
		ref    *TypeRef
	}
	var tys []typed
	var other []string
	allJSON := true
	for i := range op.Responses {
		r := &op.Responses[i]
		if r.Kind.Kind != "success" {
			continue
		}
		c := firstContent(r)
		if c == nil {
			p.bodiless = true
			continue
		}
		add := func(t typed) {
			for _, x := range tys {
				if x.goType == t.goType {
					return
				}
			}
			tys = append(tys, t)
		}
		switch c.Encoding {
		case "json":
			add(typed{g.goTypeOf(&c.Ty, append(append([]string(nil), hint...), "response")), &c.Ty})
		case "jsonl":
			array := &TypeRef{Inline: &Shape{Kind: "array", Items: &c.Ty}}
			add(typed{g.goTypeOf(array, append(append([]string(nil), hint...), "response")), array})
		case "text":
			allJSON = false
			other = append(other, "string")
		case "bytes":
			allJSON = false
			other = append(other, "tungsten.Binary")
		default:
			allJSON = false
			other = append(other, g.goTypeOf(&c.Ty, append(append([]string(nil), hint...), "response")))
		}
	}
	distinct := []string{}
	for _, t := range tys {
		distinct = append(distinct, t.goType)
	}
	for _, o := range other {
		found := false
		for _, d := range distinct {
			if d == o {
				found = true
			}
		}
		if !found {
			distinct = append(distinct, o)
		}
	}
	switch {
	case len(distinct) == 0 && p.bodiless:
		p.success = "tungsten.NoContent"
	case len(distinct) == 1:
		p.success = distinct[0]
	default:
		p.success = "json.RawMessage"
		if len(distinct) > 1 {
			g.diags = append(g.diags, diagnostic{Code: "GO003", Severity: "info",
				Message: "operation `" + op.ID + "` answers different bodies by status; its typed result is json.RawMessage (the decoded body stays in Response.Raw)"})
		}
	}
	if allJSON && len(tys) == 1 {
		p.successRT = tys[0].ref
		schema := g.schemaRef(tys[0].ref, append(append([]string(nil), hint...), "response"))
		if p.bodiless {
			schema = schemaLit([]string{`Kind: "nullable"`, "Inner: " + schema})
		}
		p.response = schema
	}
	if op.Pagination != nil {
		p.itemType = "json.RawMessage"
		if len(tys) > 0 {
			if items := g.itemsRef(tys[0].ref, op.Pagination.ItemsField); items != nil {
				p.itemType = g.goTypeOf(items, append(append([]string(nil), hint...), "item"))
				p.itemSchema = g.schemaRef(items, append(append([]string(nil), hint...), "item"))
			}
		}
	}
	if op.Stream != nil {
		p.streamVar = g.names.alloc("Stream" + goName(splitWords(op.ID)))
		ev := &op.Stream.Event
		if ev.Inline != nil && ev.Inline.Kind == "any" {
			p.eventType = "json.RawMessage"
		} else {
			p.eventType = g.goTypeOf(ev, append(append([]string(nil), hint...), "event"))
			p.eventSchema = g.schemaRef(ev, append(append([]string(nil), hint...), "event"))
		}
	}
}

// itemsRef is the item type of a page: the items of the array at
// itemsField of the body (the body itself when the field is empty).
func (g *gen) itemsRef(body *TypeRef, itemsField string) *TypeRef {
	list := body
	if itemsField != "" {
		s := g.ir.Resolve(body)
		if s == nil || s.Kind != "record" {
			return nil
		}
		list = nil
		for i := range s.Fields {
			if s.Fields[i].WireName == itemsField {
				list = &s.Fields[i].Ty
			}
		}
		if list == nil {
			return nil
		}
	}
	s := g.ir.Resolve(list)
	if s == nil || s.Kind != "array" || s.Items == nil {
		return nil
	}
	return s.Items
}

// sensitiveRequestFields are the dotted argument paths of sensitive body
// fields (merged fields by argument key, nested fields by wire name).
func (g *gen) sensitiveRequestFields(p *opPlan) []string {
	b := p.body
	if b == nil {
		return nil
	}
	switch b.content.Encoding {
	case "bytes", "text", "jsonl":
		return nil
	}
	var out []string
	active := map[string]bool{}
	if b.merged != nil {
		for _, f := range b.merged {
			if f.Sensitive {
				out = append(out, f.WireName)
			} else {
				g.sensitivePaths(&f.Ty, f.WireName, active, &out)
			}
		}
	} else {
		g.sensitivePaths(&b.content.Ty, b.arg, active, &out)
	}
	sort.Strings(out)
	var unique []string
	for i, s := range out {
		if i == 0 || out[i-1] != s {
			unique = append(unique, s)
		}
	}
	return unique
}

func (g *gen) sensitivePaths(t *TypeRef, prefix string, active map[string]bool, out *[]string) {
	if strings.Count(prefix, ".") >= 16 || len(*out) >= 256 {
		return
	}
	if t.Inline == nil {
		if active[t.Named] {
			return
		}
		active[t.Named] = true
		defer delete(active, t.Named)
	}
	s := g.ir.Resolve(t)
	if s == nil {
		return
	}
	switch s.Kind {
	case "record":
		for i := range s.Fields {
			f := &s.Fields[i]
			path := prefix + "." + f.WireName
			if f.Sensitive {
				*out = append(*out, path)
			} else {
				g.sensitivePaths(&f.Ty, path, active, out)
			}
		}
	case "nullable":
		if s.Inner != nil {
			g.sensitivePaths(s.Inner, prefix, active, out)
		}
	case "array":
		if s.Items != nil {
			g.sensitivePaths(s.Items, prefix, active, out)
		}
	case "union":
		for i := range s.Variants {
			g.sensitivePaths(&s.Variants[i].Ty, prefix, active, out)
		}
	case "intersection":
		for i := range s.Members {
			g.sensitivePaths(&s.Members[i], prefix, active, out)
		}
	}
}
