// SPDX-License-Identifier: Apache-2.0

package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"math"
	"strconv"
	"strings"
)

// goType is one Go type the SDK declares: a named IR type, or an inline
// record, union or enum that needs a name.
type goType struct {
	name   string
	shape  *Shape
	doc    *Doc
	id     string // the IR type id, "" for an inline type
	schema string // the schema variable
	// recursive: the type takes part in a reference cycle.
	recursive bool
}

// scope allocates the package's top-level identifiers.
type scope struct {
	taken map[string]bool
}

func newScope(reserved ...string) *scope {
	s := &scope{taken: map[string]bool{}}
	for _, r := range reserved {
		s.taken[r] = true
	}
	return s
}

// alloc returns name, or name2, name3, ... when it is taken.
func (s *scope) alloc(name string) string {
	if !s.taken[name] {
		s.taken[name] = true
		return name
	}
	for n := 2; ; n++ {
		candidate := name + strconv.Itoa(n)
		if !s.taken[candidate] {
			s.taken[candidate] = true
			return candidate
		}
	}
}

// Identifiers every generated package declares itself.
var packageReserved = []string{
	"Client", "New", "API", "Operations", "Macros", "Version", "ClientOptions", "CallOptions",
	"init", "main",
}

// planTypes names every named type, in IR order.
func (g *gen) planTypes() {
	for i := range g.ir.Types.Types {
		nt := &g.ir.Types.Types[i]
		words := nt.Name.Words
		if ns := splitWords(nt.Namespace); g.multi && !hasPrefixWords(words, ns) {
			words = append(ns, words...)
		}
		name := g.names.alloc(goName(words))
		t := &goType{name: name, shape: &nt.Shape, doc: nt.Doc, id: nt.ID, recursive: nt.Recursive}
		t.schema = g.names.alloc("schema" + name)
		g.types = append(g.types, t)
		g.byID[nt.ID] = t
	}
}

// inlineType gives an inline record, union or enum a Go name from hint.
func (g *gen) inlineType(s *Shape, hint []string) *goType {
	if t, ok := g.inline[s]; ok {
		return t
	}
	name := g.names.alloc(goName(hint))
	t := &goType{name: name, shape: s, schema: g.names.alloc("schema" + name)}
	g.inline[s] = t
	g.types = append(g.types, t)
	return t
}

// needsName: shapes that become a Go declaration of their own.
func needsName(s *Shape) bool {
	switch s.Kind {
	case "record", "union":
		return true
	}
	return false
}

// pointerLike: types whose zero value is nil, so they carry null
// themselves.
func pointerLike(t string) bool {
	return strings.HasPrefix(t, "[]") || strings.HasPrefix(t, "map[") || t == "json.RawMessage" || strings.HasPrefix(t, "*")
}

func primitiveType(p Primitive) string {
	switch p.Kind {
	case "string":
		if p.FormatName() == "byte" {
			return "[]byte"
		}
		return "string"
	case "int32":
		return "int32"
	case "int64", "integer":
		return "int64"
	case "float", "double", "number":
		return "float64"
	case "bool":
		return "bool"
	case "bytes":
		return "tungsten.Binary"
	}
	return "json.RawMessage"
}

// valueKind is the Go type of a JSON literal.
func valueKind(raw json.RawMessage) string {
	var v any
	if json.Unmarshal(raw, &v) != nil {
		return "json.RawMessage"
	}
	switch t := v.(type) {
	case string:
		return "string"
	case bool:
		return "bool"
	case float64:
		if t == math.Trunc(t) {
			return "int64"
		}
		return "float64"
	}
	return "json.RawMessage"
}

func enumBase(s *Shape) string {
	base := primitiveType(s.Base)
	for _, v := range s.Values {
		k := valueKind(v.Value)
		if k == "int64" && base == "float64" {
			continue
		}
		if k != base {
			return "json.RawMessage"
		}
	}
	return base
}

// goTypeOf is the Go type of a type reference; hint names inline types.
func (g *gen) goTypeOf(t *TypeRef, hint []string) string {
	if t.Inline == nil {
		nt, ok := g.byID[t.Named]
		if !ok {
			return "json.RawMessage"
		}
		return nt.name
	}
	s := t.Inline
	if needsName(s) {
		return g.inlineType(s, hint).name
	}
	return g.shapeType(s, hint)
}

// shapeType is the Go type expression of a shape that needs no name of its
// own (the right-hand side of a named type's declaration too).
func (g *gen) shapeType(s *Shape, hint []string) string {
	switch s.Kind {
	case "primitive":
		return primitiveType(s.Primitive)
	case "enum":
		return enumBase(s)
	case "const":
		return valueKind(s.Value)
	case "array":
		if s.Items == nil {
			return "[]json.RawMessage"
		}
		return "[]" + g.goTypeOf(s.Items, append(hint, "item"))
	case "map":
		if s.MapValues == nil {
			return "map[string]json.RawMessage"
		}
		return "map[string]" + g.goTypeOf(s.MapValues, append(hint, "value"))
	case "nullable":
		if s.Inner == nil {
			return "json.RawMessage"
		}
		inner := g.goTypeOf(s.Inner, hint)
		if pointerLike(inner) {
			return inner
		}
		return "*" + inner
	}
	return "json.RawMessage"
}

// recursiveRef reports whether a field type refers to a recursive named
// record or union, which a struct must hold by pointer.
func (g *gen) recursiveRef(t *TypeRef) bool {
	if t.Inline != nil {
		return false
	}
	nt, ok := g.byID[t.Named]
	return ok && nt.recursive && needsName(nt.shape)
}

// fieldPlan is one struct field.
type fieldPlan struct {
	goName string
	field  *Field
	goType string
	tag    string
	patch  bool
}

func (g *gen) fieldPlans(t *goType) []fieldPlan {
	s := t.shape
	words := make([][]string, len(s.Fields))
	for i := range s.Fields {
		words[i] = s.Fields[i].Name.Words
	}
	names := goNames(words, "AdditionalProperties", "MarshalJSON", "UnmarshalJSON")
	out := make([]fieldPlan, len(s.Fields))
	for i := range s.Fields {
		f := &s.Fields[i]
		base := g.goTypeOf(&f.Ty, append(splitWords(t.name), f.Name.Words...))
		var typ, tag string
		patch := false
		switch f.Presence {
		case "required":
			typ, tag = base, f.WireName
			if g.recursiveRef(&f.Ty) {
				typ = "*" + base
			}
		case "required_nullable":
			typ, tag = base, f.WireName
			if !pointerLike(base) {
				typ = "*" + base
			}
		case "optional_nullable":
			typ, tag, patch = "*tungsten.Patch["+base+"]", f.WireName+",omitempty", true
		default:
			typ, tag = base, f.WireName+",omitempty"
			if base != "json.RawMessage" && !strings.HasPrefix(base, "*") {
				typ = "*" + base
			}
		}
		out[i] = fieldPlan{goName: names[i], field: f, goType: typ, tag: tag, patch: patch}
	}
	return out
}

func docComment(w *writer, name string, doc *Doc, extra ...string) {
	var parts []string
	if doc != nil {
		text := strings.TrimSpace(doc.Description)
		if text == "" {
			text = strings.TrimSpace(doc.Summary)
		}
		if text != "" {
			parts = append(parts, text)
		}
	}
	parts = append(parts, extra...)
	if len(parts) == 0 {
		return
	}
	text := strings.Join(parts, "\n\n")
	if name != "" && !strings.HasPrefix(text, name+" ") {
		text = name + ": " + text
	}
	w.comment(text)
}

// writeType declares one Go type.
func (g *gen) writeType(w *writer, t *goType) {
	s := t.shape
	switch s.Kind {
	case "record":
		g.writeRecord(w, t)
	case "union":
		g.writeUnion(w, t)
	case "enum":
		base := enumBase(s)
		docComment(w, t.name, t.doc)
		if base == "json.RawMessage" {
			w.line("type %s = json.RawMessage", t.name)
			return
		}
		w.line("type %s %s", t.name, base)
		if len(s.Values) == 0 {
			return
		}
		consts := g.enumConsts(t)
		w.line("")
		w.line("// The values of %s.", t.name)
		w.line("const (")
		for i, v := range s.Values {
			w.line("%s %s = %s", consts[i], t.name, goLiteral(v.Value, base))
		}
		w.line(")")
	default:
		docComment(w, t.name, t.doc)
		w.line("type %s = %s", t.name, g.shapeType(s, splitWords(t.name)))
	}
}

// enumConsts names the constants of an enum type once.
func (g *gen) enumConsts(t *goType) []string {
	if names, ok := g.consts[t]; ok {
		return names
	}
	names := make([]string, len(t.shape.Values))
	for i, v := range t.shape.Values {
		names[i] = g.names.alloc(goName(append(splitWords(t.name), v.Name.Words...)))
	}
	g.consts[t] = names
	return names
}

// goLiteral is a JSON literal as a Go constant of kind.
func goLiteral(raw json.RawMessage, kind string) string {
	var v any
	if json.Unmarshal(raw, &v) != nil {
		return `""`
	}
	switch t := v.(type) {
	case string:
		return goString(t)
	case bool:
		return strconv.FormatBool(t)
	case float64:
		if kind == "int64" || kind == "int32" {
			return strconv.FormatInt(int64(t), 10)
		}
		return strconv.FormatFloat(t, 'g', -1, 64)
	}
	return `""`
}

func (g *gen) writeRecord(w *writer, t *goType) {
	s := t.shape
	fields := g.fieldPlans(t)
	docComment(w, t.name, t.doc)
	w.line("type %s struct {", t.name)
	for _, f := range fields {
		var notes []string
		if f.field.Deprecated {
			notes = append(notes, "Deprecated: the API marks this field deprecated.")
		}
		if f.field.ReadOnly {
			notes = append(notes, "Read-only: set by the server.")
		}
		if f.field.Sensitive {
			notes = append(notes, "Sensitive: never log it.")
		}
		docComment(w, f.goName, f.field.Doc, notes...)
		w.line("%s %s `json:%s`", f.goName, f.goType, goString(f.tag))
	}
	extra := ""
	if s.Additional.Kind == "typed" && s.Additional.Values != nil {
		extra = g.goTypeOf(s.Additional.Values, append(splitWords(t.name), "extra"))
		w.comment("AdditionalProperties holds the members the API does not name.")
		w.line("AdditionalProperties map[string]%s `json:\"-\"`", extra)
	}
	w.line("}")
	var patches []fieldPlan
	for _, f := range fields {
		if f.patch {
			patches = append(patches, f)
		}
	}
	if extra == "" && len(patches) == 0 {
		return
	}
	w.line("")
	if extra != "" {
		w.comment("MarshalJSON writes the named members, then the additional ones.")
		w.line("func (m %s) MarshalJSON() ([]byte, error) {", t.name)
		w.line("type plain %s", t.name)
		w.line("return tungsten.MarshalWithExtra(plain(m), m.AdditionalProperties)")
		w.line("}")
		w.line("")
	}
	if extra != "" {
		w.comment("UnmarshalJSON reads the named members and keeps the additional ones.")
	} else {
		w.comment("UnmarshalJSON tells an explicit null from an absent member.")
	}
	w.line("func (m *%s) UnmarshalJSON(data []byte) error {", t.name)
	w.line("type plain %s", t.name)
	w.line("var p plain")
	w.line("if err := json.Unmarshal(data, &p); err != nil {")
	w.line("return err")
	w.line("}")
	if len(patches) > 0 {
		w.line("nulls := tungsten.NullMembers(data)")
		for _, f := range patches {
			inner := strings.TrimSuffix(strings.TrimPrefix(f.goType, "*tungsten.Patch["), "]")
			w.line("if nulls[%s] {", goString(f.field.WireName))
			w.line("p.%s = tungsten.Null[%s]()", f.goName, inner)
			w.line("}")
		}
	}
	if extra != "" {
		known := make([]string, len(fields))
		for i, f := range fields {
			known[i] = goString(f.field.WireName)
		}
		w.line("extra, err := tungsten.ExtraMembers[%s](data, %s)", extra, "[]string{"+strings.Join(known, ", ")+"}")
		w.line("if err != nil {")
		w.line("return err")
		w.line("}")
		w.line("p.AdditionalProperties = extra")
	}
	w.line("*m = %s(p)", t.name)
	w.line("return nil")
	w.line("}")
}

func (g *gen) unionVariants(t *goType) ([]string, []string) {
	s := t.shape
	words := make([][]string, len(s.Variants))
	for i, v := range s.Variants {
		words[i] = v.Name.Words
	}
	names := goNames(words, "MarshalJSON", "UnmarshalJSON", "Value")
	types := make([]string, len(s.Variants))
	for i := range s.Variants {
		v := &s.Variants[i]
		base := g.goTypeOf(&v.Ty, append(splitWords(t.name), v.Name.Words...))
		if pointerLike(base) {
			types[i] = base
		} else {
			types[i] = "*" + base
		}
	}
	return names, types
}

func (g *gen) writeUnion(w *writer, t *goType) {
	s := t.shape
	names, types := g.unionVariants(t)
	how := "The variant is chosen by the first one the value matches, in order."
	if s.Discriminator != nil && s.Discriminator.Property != "" {
		how = fmt.Sprintf("The variant is chosen by the %q member.", s.Discriminator.Property)
	}
	docComment(w, t.name, t.doc, "Exactly one field is set. "+how)
	w.line("type %s struct {", t.name)
	for i := range names {
		tag := ""
		if s.Variants[i].Tag != nil {
			tag = fmt.Sprintf(" (%s %q)", s.Discriminator.Property, *s.Variants[i].Tag)
		}
		w.line("// %s is the %s variant%s.", names[i], s.Variants[i].Name.Wire, tag)
		w.line("%s %s", names[i], types[i])
	}
	w.line("}")
	w.line("")
	w.line("// MarshalJSON writes the variant that is set (null when none is).")
	w.line("func (u %s) MarshalJSON() ([]byte, error) {", t.name)
	if len(names) > 0 {
		w.line("switch {")
		for _, n := range names {
			w.line("case u.%s != nil:", n)
			w.line("return json.Marshal(u.%s)", n)
		}
		w.line("}")
	}
	w.line("return []byte(\"null\"), nil")
	w.line("}")
	w.line("")
	w.line("// UnmarshalJSON reads the variant the value matches.")
	w.line("func (u *%s) UnmarshalJSON(data []byte) error {", t.name)
	w.line("*u = %s{}", t.name)
	w.line("index, err := tungsten.PickVariant(%s, data)", t.schema)
	w.line("if err != nil {")
	w.line("return err")
	w.line("}")
	if len(names) > 0 {
		w.line("switch index {")
		for i, n := range names {
			w.line("case %d:", i)
			w.line("return json.Unmarshal(data, &u.%s)", n)
		}
		w.line("}")
	}
	w.line("return nil")
	w.line("}")
}

// ------------------------------------------------------------- schemas

func (g *gen) schemaRef(t *TypeRef, hint []string) string {
	if t.Inline == nil {
		nt, ok := g.byID[t.Named]
		if !ok {
			return "&tungsten.Schema{Kind: \"any\"}"
		}
		return nt.schema
	}
	if needsName(t.Inline) {
		return g.inlineType(t.Inline, hint).schema
	}
	return g.shapeSchema(t.Inline, hint)
}

func numberLit(n *json.Number) string {
	if n == nil {
		return ""
	}
	return "tungsten.Float(" + n.String() + ")"
}

func constraintFields(c Constraints) []string {
	var parts []string
	if c.Pattern != nil {
		parts = append(parts, "Pattern: "+goString(*c.Pattern))
	}
	if c.MinLength != nil {
		parts = append(parts, fmt.Sprintf("MinLength: tungsten.Int64(%d)", *c.MinLength))
	}
	if c.MaxLength != nil {
		parts = append(parts, fmt.Sprintf("MaxLength: tungsten.Int64(%d)", *c.MaxLength))
	}
	if c.Minimum != nil {
		parts = append(parts, "Minimum: "+numberLit(c.Minimum))
	}
	if c.Maximum != nil {
		parts = append(parts, "Maximum: "+numberLit(c.Maximum))
	}
	if c.ExclusiveMinimum != nil {
		parts = append(parts, "ExclusiveMinimum: "+numberLit(c.ExclusiveMinimum))
	}
	if c.ExclusiveMaximum != nil {
		parts = append(parts, "ExclusiveMaximum: "+numberLit(c.ExclusiveMaximum))
	}
	if c.MultipleOf != nil {
		parts = append(parts, "MultipleOf: "+numberLit(c.MultipleOf))
	}
	return parts
}

func schemaLit(parts []string) string {
	return "&tungsten.Schema{" + strings.Join(parts, ", ") + "}"
}

func jsonValueLit(raw json.RawMessage) string {
	if len(raw) == 0 {
		return "nil"
	}
	return "tungsten.MustJSON(" + goString(compactJSON(raw)) + ")"
}

func compactJSON(raw json.RawMessage) string {
	var b bytes.Buffer
	if err := json.Compact(&b, raw); err != nil {
		return string(raw)
	}
	return b.String()
}

// shapeSchema is the schema literal of a shape (the value of a named type's
// schema variable too).
func (g *gen) shapeSchema(s *Shape, hint []string) string {
	switch s.Kind {
	case "primitive":
		var parts []string
		switch s.Primitive.Kind {
		case "string":
			parts = append(parts, `Kind: "string"`)
			if f := s.Primitive.FormatName(); f != "" && f != "byte" && f != "password" {
				parts = append(parts, "Format: "+goString(f))
			}
		case "int32":
			parts = append(parts, `Kind: "integer"`, "Bits: 32")
		case "int64", "integer":
			parts = append(parts, `Kind: "integer"`)
		case "float", "double", "number":
			parts = append(parts, `Kind: "number"`)
		case "bool":
			parts = append(parts, `Kind: "boolean"`)
		case "bytes":
			parts = append(parts, `Kind: "binary"`)
		default:
			parts = append(parts, `Kind: "any"`)
		}
		return schemaLit(append(parts, constraintFields(s.Constraints)...))
	case "enum":
		values := make([]string, len(s.Values))
		for i, v := range s.Values {
			values[i] = compactJSON(v.Value)
		}
		return schemaLit([]string{`Kind: "enum"`, "Values: tungsten.MustJSONList(" + goString("["+strings.Join(values, ",")+"]") + ")"})
	case "const":
		return schemaLit([]string{`Kind: "const"`, "Values: tungsten.MustJSONList(" + goString("["+compactJSON(s.Value)+"]") + ")"})
	case "array":
		parts := []string{`Kind: "array"`}
		if s.Items != nil {
			parts = append(parts, "Items: "+g.schemaRef(s.Items, append(hint, "item")))
		}
		if s.Min != nil {
			parts = append(parts, fmt.Sprintf("MinItems: tungsten.Int64(%d)", *s.Min))
		}
		if s.Max != nil {
			parts = append(parts, fmt.Sprintf("MaxItems: tungsten.Int64(%d)", *s.Max))
		}
		if s.Unique {
			parts = append(parts, "Unique: true")
		}
		return schemaLit(parts)
	case "map":
		parts := []string{`Kind: "map"`}
		if s.MapValues != nil {
			parts = append(parts, "Items: "+g.schemaRef(s.MapValues, append(hint, "value")))
		}
		return schemaLit(parts)
	case "record":
		var fields []string
		for i := range s.Fields {
			f := &s.Fields[i]
			fields = append(fields, g.fieldSchema(f.WireName, g.schemaRef(&f.Ty, append(hint, f.Name.Words...)), fieldConstraints(f), f.Required(), f.Nullable()))
		}
		parts := []string{`Kind: "object"`}
		if len(fields) > 0 {
			parts = append(parts, "Fields: []tungsten.SchemaField{"+strings.Join(fields, ", ")+"}")
		}
		switch s.Additional.Kind {
		case "closed":
			parts = append(parts, `Additional: "closed"`)
		case "typed":
			if s.Additional.Values != nil {
				parts = append(parts, "Extra: "+g.schemaRef(s.Additional.Values, append(hint, "extra")))
			}
		default:
			parts = append(parts, `Additional: "open"`)
		}
		return schemaLit(parts)
	case "union":
		var variants, tags []string
		for i := range s.Variants {
			v := &s.Variants[i]
			variants = append(variants, g.schemaRef(&v.Ty, append(hint, v.Name.Words...)))
			if v.Tag != nil {
				tags = append(tags, goString(*v.Tag))
			}
		}
		parts := []string{`Kind: "union"`}
		if len(variants) > 0 {
			parts = append(parts, "Variants: []*tungsten.Schema{"+strings.Join(variants, ", ")+"}")
		}
		if s.Discriminator != nil && s.Discriminator.Property != "" && s.Strategy == "tagged" && len(tags) == len(variants) {
			parts = append(parts, "Tag: "+goString(s.Discriminator.Property), "Tags: []string{"+strings.Join(tags, ", ")+"}")
		}
		return schemaLit(parts)
	case "intersection":
		var members []string
		for i := range s.Members {
			members = append(members, g.schemaRef(&s.Members[i], hint))
		}
		return schemaLit([]string{`Kind: "all"`, "Variants: []*tungsten.Schema{" + strings.Join(members, ", ") + "}"})
	case "nullable":
		if s.Inner == nil {
			return schemaLit([]string{`Kind: "any"`})
		}
		return schemaLit([]string{`Kind: "nullable"`, "Inner: " + g.schemaRef(s.Inner, hint)})
	case "never":
		return schemaLit([]string{`Kind: "never"`})
	}
	return schemaLit([]string{`Kind: "any"`})
}

// fieldSchema is one SchemaField literal; field-level constraints become a
// "limits" check next to the type.
func (g *gen) fieldSchema(name, schema string, c Constraints, required, nullable bool) string {
	if !c.Empty() {
		schema = schemaLit([]string{`Kind: "all"`, "Variants: []*tungsten.Schema{" + schema + ", " + schemaLit(append([]string{`Kind: "limits"`}, constraintFields(c)...)) + "}"})
	}
	parts := []string{"Name: " + goString(name), "Schema: " + schema}
	if required {
		parts = append(parts, "Required: true")
	}
	if nullable {
		parts = append(parts, "Nullable: true")
	}
	return "{" + strings.Join(parts, ", ") + "}"
}

// hasPrefixWords reports whether words start with prefix.
func hasPrefixWords(words, prefix []string) bool {
	if len(prefix) == 0 || len(words) < len(prefix) {
		return false
	}
	for i := range prefix {
		if words[i] != prefix[i] {
			return false
		}
	}
	return true
}

// fieldConstraints are the constraints written on a field, unless its
// inline primitive type carries them already.
func fieldConstraints(f *Field) Constraints {
	if f.Ty.Inline != nil && f.Ty.Inline.Kind == "primitive" && !f.Ty.Inline.Constraints.Empty() {
		return Constraints{}
	}
	return f.Constraints
}
