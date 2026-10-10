// SPDX-License-Identifier: Apache-2.0

package main

import (
	"encoding/json"
	"fmt"
	"strings"
)

// macroPlan is one macro the SDK carries as data.
type macroPlan struct {
	m       *Macro
	varName string
	method  string
	extends string
	steps   []macroStep
}

type macroStep struct {
	kind      string
	operation string
	args      json.RawMessage
	as        string
	until     json.RawMessage
	interval  *int64
	budget    json.RawMessage
	maxPages  *int64
}

func rawField(obj map[string]json.RawMessage, key string) json.RawMessage {
	v, ok := obj[key]
	if !ok {
		return nil
	}
	return v
}

func optUint(obj map[string]json.RawMessage, key string, i int) (*int64, error) {
	v := rawField(obj, key)
	if isNull(v) {
		return nil, nil
	}
	var n int64
	if err := json.Unmarshal(v, &n); err != nil || n < 0 {
		return nil, fmt.Errorf("step %d has an invalid `%s`", i, key)
	}
	return &n, nil
}

// parseMacro checks the canonical form (tungsten_ir::Macro) the way the
// TypeScript emitter does; a macro that does not parse is left out.
func (g *gen) parseMacro(m *Macro) (*macroPlan, error) {
	var raw []map[string]json.RawMessage
	if err := json.Unmarshal(m.Steps, &raw); err != nil || len(raw) == 0 {
		return nil, fmt.Errorf("`steps` must be a non-empty array")
	}
	plan := &macroPlan{m: m}
	var names []string
	for i, obj := range raw {
		var kind, operation string
		_ = json.Unmarshal(rawField(obj, "kind"), &kind)
		switch kind {
		case "call", "poll", "paginate":
		default:
			return nil, fmt.Errorf("step %d has unknown kind %q", i, kind)
		}
		if json.Unmarshal(rawField(obj, "operation"), &operation) != nil || operation == "" {
			return nil, fmt.Errorf("step %d has no operation", i)
		}
		if _, ok := g.byOp[operation]; !ok {
			return nil, fmt.Errorf("step %d calls `%s`, which is not a callable operation", i, operation)
		}
		as := ""
		if v := rawField(obj, "as"); !isNull(v) {
			if json.Unmarshal(v, &as) != nil || as == "" || strings.Contains(as, ".") || as == "input" {
				return nil, fmt.Errorf("step %d has an invalid `as` %s", i, string(v))
			}
			for _, n := range names {
				if n == as {
					return nil, fmt.Errorf("step %d reuses the name `%s`", i, as)
				}
			}
		}
		budget := rawField(obj, "budget_ms")
		if !isNull(budget) {
			var n uint64
			var s string
			if json.Unmarshal(budget, &n) != nil && (json.Unmarshal(budget, &s) != nil || !strings.HasPrefix(s, "$")) {
				return nil, fmt.Errorf("step %d has an invalid `budget_ms`", i)
			}
		}
		until := rawField(obj, "until")
		if kind == "poll" {
			var u map[string]json.RawMessage
			if json.Unmarshal(until, &u) != nil || u == nil {
				return nil, fmt.Errorf("poll step %d has no `until` predicate", i)
			}
		} else {
			until = nil
		}
		interval, err := optUint(obj, "interval_ms", i)
		if err != nil {
			return nil, err
		}
		maxPages, err := optUint(obj, "max_pages", i)
		if err != nil {
			return nil, err
		}
		plan.steps = append(plan.steps, macroStep{
			kind: kind, operation: operation, args: g.stepArgs(operation, rawField(obj, "args")), as: as,
			until: until, interval: interval, budget: budget, maxPages: maxPages,
		})
		names = append(names, as)
	}
	var input map[string]json.RawMessage
	_ = json.Unmarshal(m.Input, &input)
	if v := rawField(input, "extends"); !isNull(v) {
		var id string
		if json.Unmarshal(v, &id) != nil {
			return nil, fmt.Errorf("input `extends` is invalid: %s", string(v))
		}
		if _, ok := g.byOp[id]; !ok {
			return nil, fmt.Errorf("input extends `%s`, which is not a callable operation", id)
		}
		plan.extends = id
	} else {
		for i, obj := range raw {
			var s string
			if json.Unmarshal(rawField(obj, "args"), &s) == nil && s == "$input" {
				plan.extends = plan.steps[i].operation
				break
			}
		}
	}
	if v := rawField(input, "add"); !isNull(v) {
		var add map[string]json.RawMessage
		if json.Unmarshal(v, &add) != nil {
			return nil, fmt.Errorf("input `add` is invalid: %s", string(v))
		}
	}
	return plan, nil
}

// stepArgs renames the keys of an argument object that are a parameter's
// wire name to that parameter's argument key.
func (g *gen) stepArgs(operation string, args json.RawMessage) json.RawMessage {
	if isNull(args) {
		return json.RawMessage("{}")
	}
	p := g.byOp[operation]
	if !strings.HasPrefix(strings.TrimSpace(string(args)), "{") {
		return args
	}
	out, err := renameKeys(args, func(k string) string {
		for _, a := range p.params {
			if isArg(a.param) && a.param.WireName == k {
				return a.key
			}
		}
		return k
	})
	if err != nil {
		return args
	}
	return out
}

func (g *gen) planMacros(accessors []string) {
	words := [][]string{}
	var plans []*macroPlan
	for i := range g.ir.Agent.Macros {
		m := &g.ir.Agent.Macros[i]
		plan, err := g.parseMacro(m)
		if err != nil {
			g.diags = append(g.diags, diagnostic{Code: "GO001", Severity: "warning",
				Message: fmt.Sprintf("macro `%s` is not emitted in the Go SDK: %s", m.Name, err),
				Help:    "Macros use the canonical form documented on tungsten_ir::Macro."})
			continue
		}
		name := m.Name
		if _, rest, ok := strings.Cut(name, "."); ok {
			name = rest
		}
		words = append(words, splitWords(name))
		plans = append(plans, plan)
	}
	reserved := append(append([]string{}, clientMembers...), accessors...)
	methods := goNames(words, reserved...)
	// The previews of macros share the method set too.
	taken := map[string]bool{}
	for _, r := range reserved {
		taken[r] = true
	}
	for _, m := range methods {
		taken[m] = true
	}
	for i, p := range plans {
		p.method = methods[i]
		if taken["Preview"+p.method] && p.m.Safety != "read_only" {
			p.method += "Macro"
		}
		p.varName = g.names.alloc("Macro" + methods[i])
		g.macros = append(g.macros, p)
	}
}

var dispatchMethods = []string{"Operations", "Macros", "Invoke", "PreviewOperation", "Paginate", "StreamEvents", "RunMacro", "PreviewMacro"}

func macroDescriptorLit(p *macroPlan) string {
	m := p.m
	var b strings.Builder
	line := func(format string, args ...any) {
		fmt.Fprintf(&b, format, args...)
		b.WriteByte('\n')
	}
	line("&tungsten.MacroDescriptor{")
	line("Name: %s,", goString(m.Name))
	line("Summary: %s,", goString(m.Summary))
	line("Safety: %s,", rt(safeties, m.Safety, "Mutating"))
	line("Steps: []tungsten.MacroStep{")
	for _, s := range p.steps {
		parts := []string{"Kind: " + goString(s.kind), "Operation: " + goString(s.operation), "Args: " + jsonValueLit(s.args)}
		if s.as != "" {
			parts = append(parts, "As: "+goString(s.as))
		}
		if !isNull(s.until) {
			parts = append(parts, "Until: "+jsonValueLit(s.until))
		}
		if s.interval != nil {
			parts = append(parts, fmt.Sprintf("IntervalMs: tungsten.Ptr[int64](%d)", *s.interval))
		}
		if !isNull(s.budget) {
			parts = append(parts, "BudgetMs: "+jsonValueLit(s.budget))
		}
		if s.maxPages != nil {
			parts = append(parts, fmt.Sprintf("MaxPages: tungsten.Ptr[int64](%d)", *s.maxPages))
		}
		line("{%s},", strings.Join(parts, ", "))
	}
	line("},")
	if !isNull(m.Output) {
		line("Output: %s,", jsonValueLit(m.Output))
	}
	var input map[string]json.RawMessage
	_ = json.Unmarshal(m.Input, &input)
	in := []string{}
	if p.extends != "" {
		in = append(in, "Extends: "+goString(p.extends))
	}
	if add := rawField(input, "add"); !isNull(add) {
		in = append(in, "Add: tungsten.MustJSONObject("+goString(compactJSON(add))+")")
	}
	line("Input: tungsten.MacroInput{%s},", strings.Join(in, ", "))
	if len(m.SensitiveResponseFields) > 0 {
		line("SensitiveResponseFields: %s,", stringList(m.SensitiveResponseFields))
	}
	if m.ShownOnce {
		line("ShownOnce: true,")
	}
	if m.Cluster != "" {
		line("Cluster: %s,", goString(m.Cluster))
	}
	b.WriteString("}")
	return b.String()
}
