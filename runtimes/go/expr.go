// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"regexp"
	"strings"
)

// Predicates (verify expect and terminal, poll until): field path to
// {"in": [...]} | {"equals": x} | {"contains": {...}}; any other value is
// compared for equality. Every entry must hold.
//
// Expressions: JSON in which a string starting with "$" is a reference
// resolved against a scope ($input.a.b, $<name>.a, $response.x, $args.y),
// an object {"expr": "<ref> in [..]" | "<ref> == x" | "<ref> != x"} is a
// boolean, anything else a literal. A dry evaluation (macro previews)
// knows the input but not the earlier results: a reference to one is the
// placeholder "<from step NAME: path>".

// scope binds names (without "$") to values.
type scope = *Object

// EvaluatePredicate reports whether every entry of predicate holds on the
// value. A null predicate always holds; a malformed one never does.
func EvaluatePredicate(predicate any, value any, present bool) bool {
	switch p := predicate.(type) {
	case nil:
		return true
	case *Object:
		for _, path := range p.keys {
			actual, ok := getPathStr(value, present, path)
			if !holds(p.vals[path], actual, ok) {
				return false
			}
		}
		return true
	}
	return false
}

func holds(test any, actual any, present bool) bool {
	if m, ok := test.(*Object); ok && m.Len() == 1 {
		if candidates, ok := m.Get("in"); ok {
			list, isList := candidates.([]any)
			if !isList {
				return false
			}
			for _, c := range list {
				if deepEqualOpt(c, true, actual, present) {
					return true
				}
			}
			return false
		}
		if expected, ok := m.Get("equals"); ok {
			return deepEqualOpt(expected, true, actual, present)
		}
		if pattern, ok := m.Get("contains"); ok {
			if text, isText := pattern.(string); isText {
				switch have := actual.(type) {
				case string:
					return present && strings.Contains(have, text)
				case []any:
					for _, item := range have {
						if s, ok := item.(string); ok && s == text {
							return true
						}
					}
				}
				return false
			}
			if items, isList := actual.([]any); present && isList {
				if _, patternIsList := pattern.([]any); !patternIsList {
					for _, item := range items {
						if containsSubset(item, true, pattern) {
							return true
						}
					}
					return false
				}
			}
			return containsSubset(actual, present, pattern)
		}
	}
	return deepEqualOpt(test, true, actual, present)
}

// resolveRef resolves one $name.path reference.
func resolveRef(reference string, sc scope) (any, bool) {
	segments := splitPath(strings.TrimPrefix(reference, "$"))
	if len(segments) == 0 {
		return nil, false
	}
	bound, ok := sc.Get(segments[0])
	if !ok {
		return nil, false
	}
	return getPath(bound, true, segments[1:])
}

// evaluateExpr evaluates an expression; unknown references are absent
// (dropped from objects); a malformed expr string is null.
func evaluateExpr(expr any, sc scope, depth int) (any, bool) {
	if depth > 64 {
		return nil, true
	}
	switch t := expr.(type) {
	case string:
		if strings.HasPrefix(t, "$") {
			return resolveRef(t, sc)
		}
		return t, true
	case []any:
		out := make([]any, len(t))
		for i, item := range t {
			v, ok := evaluateExpr(item, sc, depth+1)
			if ok {
				out[i] = v
			}
		}
		return out, true
	case *Object:
		if t.Len() == 1 {
			if source, ok := t.vals["expr"].(string); ok {
				return evaluateBoolean(source, sc), true
			}
		}
		out := NewObject()
		for _, k := range t.keys {
			if v, ok := evaluateExpr(t.vals[k], sc, depth+1); ok {
				out.Set(k, v)
			}
		}
		return out, true
	}
	return expr, true
}

func placeholder(name, path string) string {
	if path == "" {
		return "<from step " + name + ">"
	}
	return "<from step " + name + ": " + path + ">"
}

var placeholderPattern = regexp.MustCompile(`^<from step [^<>]+>$`)

func placeholdersIn(v any, out *[]string, depth int) {
	if depth > 64 {
		return
	}
	switch t := v.(type) {
	case string:
		if placeholderPattern.MatchString(t) {
			*out = append(*out, t)
		}
	case []any:
		for _, item := range t {
			placeholdersIn(item, out, depth+1)
		}
	case *Object:
		for _, k := range t.keys {
			placeholdersIn(t.vals[k], out, depth+1)
		}
	}
}

func containsPlaceholder(v any) bool {
	var found []string
	placeholdersIn(v, &found, 0)
	return len(found) > 0
}

// pendingRef returns the pending binding a reference names and the path
// written after it.
func pendingRef(reference string, pending map[string]bool) (string, string, bool) {
	body, ok := strings.CutPrefix(reference, "$")
	if !ok {
		return "", "", false
	}
	segs := splitPath(body)
	if len(segs) == 0 {
		return "", "", false
	}
	name := segs[0]
	if !pending[name] || !strings.HasPrefix(reference, "$"+name) {
		return "", "", false
	}
	rest := reference[len(name)+1:]
	return name, strings.TrimPrefix(rest, "."), true
}

var dryRef = regexp.MustCompile(`^\$[^\s=!]+`)

func evaluateDry(expr any, sc scope, pending map[string]bool, depth int) (any, bool) {
	if depth > 64 {
		return nil, true
	}
	switch t := expr.(type) {
	case string:
		if strings.HasPrefix(t, "$") {
			if name, path, ok := pendingRef(t, pending); ok {
				return placeholder(name, path), true
			}
			return resolveRef(t, sc)
		}
		return t, true
	case []any:
		out := make([]any, len(t))
		for i, item := range t {
			if v, ok := evaluateDry(item, sc, pending, depth+1); ok {
				out[i] = v
			}
		}
		return out, true
	case *Object:
		if t.Len() == 1 {
			if raw, ok := t.vals["expr"].(string); ok {
				source := strings.TrimSpace(raw)
				head := dryRef.FindString(source)
				if name, _, ok := pendingRef(head, pending); ok {
					rest := source[len(name)+1:]
					return placeholder(name, strings.TrimPrefix(rest, ".")), true
				}
				return evaluateBoolean(source, sc), true
			}
		}
		out := NewObject()
		for _, k := range t.keys {
			if v, ok := evaluateDry(t.vals[k], sc, pending, depth+1); ok {
				out.Set(k, v)
			}
		}
		return out, true
	}
	return expr, true
}

// describePredicate is a predicate in words: state in ["a", "b"],
// endpoints contains {...}, a = 1, joined with "and".
func describePredicate(predicate any) string {
	p, ok := predicate.(*Object)
	if !ok {
		return ""
	}
	parts := make([]string, 0, p.Len())
	for _, path := range p.keys {
		test := p.vals[path]
		if m, ok := test.(*Object); ok && m.Len() == 1 {
			if list, ok := m.vals["in"].([]any); ok {
				shown := make([]string, len(list))
				for i, v := range list {
					shown[i] = JSONText(v)
				}
				parts = append(parts, path+" in ["+strings.Join(shown, ", ")+"]")
				continue
			}
			if expected, ok := m.Get("equals"); ok {
				parts = append(parts, path+" = "+JSONText(expected))
				continue
			}
			if pattern, ok := m.Get("contains"); ok {
				parts = append(parts, path+" contains "+JSONText(pattern))
				continue
			}
		}
		parts = append(parts, path+" = "+JSONText(test))
	}
	return strings.Join(parts, " and ")
}

var singleQuoted = regexp.MustCompile(`'((?:[^'\\]|\\.)*)'`)

func parseLiteral(text string) (any, bool) {
	trimmed := strings.TrimSpace(text)
	if v, err := ParseJSONString(trimmed); err == nil {
		return v, true
	}
	normalized := singleQuoted.ReplaceAllStringFunc(trimmed, func(m string) string {
		inner := m[1 : len(m)-1]
		return jsString(strings.ReplaceAll(inner, `\'`, "'"))
	})
	v, err := ParseJSONString(normalized)
	return v, err == nil
}

var booleanExpr = regexp.MustCompile(`(?s)^\s*(\$[^\s=!]+)\s+(in|==|!=)\s+(.+)$`)

func evaluateBoolean(source string, sc scope) any {
	m := booleanExpr.FindStringSubmatch(source)
	if m == nil {
		return nil
	}
	literal, ok := parseLiteral(m[3])
	if !ok {
		return nil
	}
	actual, present := resolveRef(m[1], sc)
	if m[2] == "in" {
		list, ok := literal.([]any)
		if !ok {
			return nil
		}
		for _, v := range list {
			if deepEqualOpt(v, true, actual, present) {
				return true
			}
		}
		return false
	}
	if !present {
		actual = nil
	}
	equal := deepEqual(actual, literal)
	if m[2] == "==" {
		return equal
	}
	return !equal
}
