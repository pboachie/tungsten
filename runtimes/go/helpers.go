// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"math"
	"regexp"
	"strings"
)

// argParams are the parameters the caller supplies as arguments.
func argParams(op *OperationDescriptor) []*ParamDescriptor {
	var out []*ParamDescriptor
	for i := range op.Params {
		if op.Params[i].Role == RolePlain || op.Params[i].Role == RoleDryRun {
			out = append(out, &op.Params[i])
		}
	}
	return out
}

// descriptorProblem names a structural problem that makes the descriptor
// unusable, or "".
func descriptorProblem(op *OperationDescriptor) string {
	if op.ID == "" {
		return "`id` must be a non-empty string"
	}
	for _, p := range op.Params {
		if p.Name == "" || p.Wire == "" {
			return "every parameter needs a `name` and a `wire`"
		}
	}
	if op.RPC != nil && (op.RPC.Field == "" || op.RPC.ParamsField == "") {
		return "`rpc` needs `field` and `params_field`"
	}
	return ""
}

// argumentPath is the JSON path of an argument issue: body.<wire> for a
// merged body field, body for the whole-body argument, else args.<name>.
func argumentPath(op *OperationDescriptor, path []PathSegment) string {
	if op.Body != nil && len(path) > 0 {
		head := segmentString(path[0])
		rest := path[1:]
		if op.Body.Arg != "" {
			if op.Body.Arg == head {
				return "body" + formatIssuePath(rest)
			}
		} else {
			for _, f := range op.Body.Fields {
				if f.Arg == head {
					return "body." + f.Wire + formatIssuePath(rest)
				}
			}
		}
	}
	if len(path) == 0 {
		return "args"
	}
	return "args" + formatIssuePath(path)
}

func sensitiveRequestPaths(op *OperationDescriptor) []string {
	var out []string
	for _, p := range op.Agent.SensitiveRequestFields {
		if p != "" {
			out = append(out, p)
		}
	}
	return out
}

// dottedArgPath is an argument path without array indices.
func dottedArgPath(path []PathSegment) string {
	var parts []string
	for _, seg := range path {
		if s, ok := seg.(string); ok && !isIndexOrEmptyDigits(s) {
			parts = append(parts, s)
		}
	}
	return strings.Join(parts, ".")
}

// isIndexOrEmptyDigits mirrors the Rust check: a key made only of digits
// (also the empty key) is an index.
func isIndexOrEmptyDigits(s string) bool {
	for i := 0; i < len(s); i++ {
		if s[i] < '0' || s[i] > '9' {
			return false
		}
	}
	return true
}

func sensitiveArg(op *OperationDescriptor, path []PathSegment) bool {
	if len(path) > 0 {
		head := segmentString(path[0])
		for _, p := range op.Params {
			if p.Sensitive && p.Name == head {
				return true
			}
		}
	}
	dotted := dottedArgPath(path)
	if dotted != "" {
		for _, f := range sensitiveRequestPaths(op) {
			if dotted == f || strings.HasPrefix(dotted, f+".") {
				return true
			}
		}
	}
	for _, seg := range path {
		if s, ok := seg.(string); ok && looksSensitive(s) {
			return true
		}
	}
	return false
}

// redactBelow redacts the sensitive fields below the argument at path.
func redactBelow(op *OperationDescriptor, path []PathSegment, value any) any {
	dotted := dottedArgPath(path)
	prefix := ""
	if dotted != "" {
		prefix = dotted + "."
	}
	var below []string
	for _, f := range sensitiveRequestPaths(op) {
		if rest, ok := strings.CutPrefix(f, prefix); ok {
			below = append(below, rest)
		}
	}
	if len(below) == 0 {
		return value
	}
	return redactPaths(value, below)
}

// sensitiveBodyPaths are the sensitive argument paths relative to the body
// as sent; "" is the whole body.
func sensitiveBodyPaths(op *OperationDescriptor) []string {
	paths := sensitiveRequestPaths(op)
	var relative []string
	switch {
	case op.Body != nil && op.Body.Arg == "":
		for _, p := range paths {
			segs := splitPath(p)
			if len(segs) == 0 {
				continue
			}
			for _, f := range op.Body.Fields {
				if f.Arg == segs[0] {
					rest := strings.Join(segs[1:], ".")
					if rest == "" {
						relative = append(relative, f.Wire)
					} else {
						relative = append(relative, f.Wire+"."+rest)
					}
					break
				}
			}
		}
	case op.Body != nil:
		arg := op.Body.Arg
		for _, p := range paths {
			if p == arg {
				relative = append(relative, "")
			} else if rest, ok := strings.CutPrefix(p, arg+"."); ok {
				relative = append(relative, rest)
			}
		}
	case op.RPC != nil:
		relative = append(relative, paths...)
	}
	if op.RPC == nil {
		return relative
	}
	out := make([]string, len(relative))
	for i, p := range relative {
		if p == "" {
			out[i] = op.RPC.ParamsField
		} else {
			out[i] = op.RPC.ParamsField + "." + p
		}
	}
	return out
}

// valuesAt collects every string or number found at a path, through arrays.
func valuesAt(v any, segments []string, out *[]string, depth int) {
	if depth > 64 {
		return
	}
	if list, ok := v.([]any); ok {
		for _, item := range list {
			valuesAt(item, segments, out, depth+1)
		}
		return
	}
	if len(segments) == 0 {
		switch t := v.(type) {
		case string:
			*out = append(*out, t)
		case int64, float64:
			*out = append(*out, numberText(t))
		case *Object:
			for _, k := range t.keys {
				valuesAt(t.vals[k], nil, out, depth+1)
			}
		}
		return
	}
	if o, ok := v.(*Object); ok {
		if next, ok := o.Get(segments[0]); ok {
			valuesAt(next, segments[1:], out, depth+1)
		}
	}
}

// withWireNames adds each parameter's value under its wire name, so
// references written against the API find arguments whose generated name
// differs.
func withWireNames(op *OperationDescriptor, args *Object) *Object {
	out := args.Clone()
	for _, p := range op.Params {
		if v, ok := args.Get(p.Name); ok && !out.Has(p.Wire) {
			out.Set(p.Wire, v)
		}
	}
	return out
}

// argPath rewrites a leading parameter wire name to its argument name.
func argPath(op *OperationDescriptor, path []string) []string {
	if len(path) == 0 {
		return path
	}
	for _, p := range op.Params {
		if p.Wire == path[0] && p.Name != path[0] {
			return append([]string{p.Name}, path[1:]...)
		}
	}
	return path
}

// argName is the argument name of a parameter key given by name or wire.
func argName(op *OperationDescriptor, key string) string {
	for _, p := range op.Params {
		if p.Name == key {
			return p.Name
		}
	}
	for _, p := range op.Params {
		if p.Wire == key {
			return p.Name
		}
	}
	return key
}

var placeholderRef = regexp.MustCompile(`\{([^{}]+)\}`)

// interpolate fills {field} references from the arguments; sensitive ones
// are redacted, unset ones shown as <unset>.
func interpolate(template string, args *Object, op *OperationDescriptor) string {
	return placeholderRef.ReplaceAllStringFunc(template, func(m string) string {
		inner := strings.TrimSpace(m[1 : len(m)-1])
		segments := argPath(op, strings.Split(inner, "."))
		value, ok := getPath(args, true, segments)
		if !ok || value == nil {
			return "<unset>"
		}
		path := make([]PathSegment, len(segments))
		for i, s := range segments {
			path[i] = s
		}
		if sensitiveArg(op, path) {
			return Redacted
		}
		shown := envelopeValue(value, false)
		if text, ok := shown.(string); ok {
			return text
		}
		return JSONText(shown)
	})
}

// unescapePlaceholders shows the placeholders of args in a preview URL as
// written instead of percent-encoded.
func unescapePlaceholders(url string, args any) string {
	var found []string
	placeholdersIn(args, &found, 0)
	seen := map[string]bool{}
	for _, text := range found {
		if seen[text] {
			continue
		}
		seen[text] = true
		url = strings.ReplaceAll(url, encodeComponent(text), text)
	}
	return url
}

// shortJSON is JSON for remediation text, cut to 80 characters.
func shortJSON(v any) string {
	text := []rune(JSONText(v))
	if len(text) > 80 {
		return string(text[:77]) + "..."
	}
	return string(text)
}

// withArguments is " with name value, ..." for remediation text.
func withArguments(args any) string {
	o, ok := args.(*Object)
	if !ok || o.Len() == 0 {
		return ""
	}
	shown := make([]string, 0, o.Len())
	for _, k := range o.keys {
		shown = append(shown, k+" "+shortJSON(o.vals[k]))
	}
	return " with " + strings.Join(shown, ", ")
}

// sentArguments is " (name value, ...)": the scalar, non-sensitive
// arguments a call was made with (at most four, by wire name).
func sentArguments(op *OperationDescriptor, args *Object) string {
	var shown []string
	for _, k := range args.keys {
		if len(shown) == 4 {
			break
		}
		v := args.vals[k]
		switch v.(type) {
		case string, int64, float64, bool:
		default:
			continue
		}
		if sensitiveArg(op, []PathSegment{k}) {
			continue
		}
		wire := k
		for _, p := range op.Params {
			if p.Name == k {
				wire = p.Wire
				break
			}
		}
		shown = append(shown, wire+" "+shortJSON(v))
	}
	if len(shown) == 0 {
		return ""
	}
	return " (" + strings.Join(shown, ", ") + ")"
}

// bounded is a finite number within [min, max], else the fallback.
func bounded(v any, ok bool, fallback, min, max float64) float64 {
	if !ok {
		return fallback
	}
	f, isNum := asFloat(v)
	if !isNum || math.IsNaN(f) || math.IsInf(f, 0) {
		return fallback
	}
	return math.Max(min, math.Min(max, f))
}
