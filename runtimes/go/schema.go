// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"fmt"
	"math"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"unicode/utf8"
)

// PathSegment is a member name (string) or an array index (int).
type PathSegment = any

// Issue is one validation problem: the path of the offending value and a
// message.
type Issue struct {
	Path    []PathSegment
	Message string
}

// Validator checks a JSON value before any network call (arguments) or
// after the response (bodies, page items, events). It returns the issues,
// none when the value is valid.
type Validator interface {
	Validate(value any) []Issue
}

// Schema is the runtime's own type description, generated from the IR: the
// validator of arguments, bodies, page items and events. Kind is one of
// "any", "never", "string", "integer", "number", "boolean", "binary",
// "enum", "const", "array", "map", "object", "union", "all", "nullable" or
// "limits" (field constraints checked on whatever kind the value has).
// Generated code declares named schemas as pointers and fills them in an
// init function, so recursive types refer to each other.
type Schema struct {
	Kind string
	// Format of a string: uuid, email, date-time, date, ipv4 and ipv6 are
	// checked.
	Format  string
	Pattern string
	// Bits of an integer (32 or 64; 0 for any safe integer).
	Bits             int
	MinLength        *int64
	MaxLength        *int64
	Minimum          *float64
	Maximum          *float64
	ExclusiveMinimum *float64
	ExclusiveMaximum *float64
	MultipleOf       *float64
	// Values of an enum; Values[0] of a const.
	Values []any
	// Items of an array, values of a map.
	Items    *Schema
	MinItems *int64
	MaxItems *int64
	Unique   bool
	// Fields of an object, in order.
	Fields []SchemaField
	// Additional members of an object: "open" (ignored), "closed"
	// (refused) or, with Extra set, checked against Extra.
	Additional string
	Extra      *Schema
	// Variants of a union or the members of an "all"; Tag is a tagged
	// union's discriminator member and Tags the value of each variant.
	Variants []*Schema
	Tag      string
	Tags     []string
	// Inner is the type of a nullable.
	Inner *Schema
}

// SchemaField is one member of an object schema.
type SchemaField struct {
	Name     string
	Schema   *Schema
	Required bool
	Nullable bool
}

// Validate implements Validator.
func (s *Schema) Validate(value any) []Issue {
	issues, _ := s.validateNotes(value)
	return issues
}

// patternNote is one schema pattern that could not be checked: the value was
// accepted without it. It never holds the value.
type patternNote struct {
	Path    []PathSegment
	Pattern string
	Reason  string
}

// noteValidator is a Validator that also reports the patterns it could not
// check, so the client can surface them through OnDiagnostic.
type noteValidator interface {
	validateNotes(value any) ([]Issue, []patternNote)
}

// validateNotes is Validate plus the unchecked patterns, one per path and
// pattern.
func (s *Schema) validateNotes(value any) ([]Issue, []patternNote) {
	v := validation{}
	v.check(s, value)
	out := append(v.structure, v.constraints...)
	return append(out, v.unknown...), v.notes
}

// seg is one segment of the path being checked: a member name or, with
// index true, an array index. The path is kept unboxed while walking and
// turned into PathSegments only for an issue.
type seg struct {
	name  string
	n     int
	index bool
}

type validation struct {
	structure   []Issue
	constraints []Issue
	unknown     []Issue
	// path is the path of the value being checked, pushed and popped as
	// the walk descends.
	path []seg
	// notes are the patterns that could not be checked, deduplicated by
	// path and pattern.
	notes    []patternNote
	noteSeen map[string]bool
}

// unchecked records a pattern that was accepted without being checked.
func (v *validation) unchecked(pattern, reason string) {
	path := v.at()
	key := formatIssuePath(path) + "\n" + pattern
	if v.noteSeen[key] {
		return
	}
	if v.noteSeen == nil {
		v.noteSeen = map[string]bool{}
	}
	v.noteSeen[key] = true
	v.notes = append(v.notes, patternNote{Path: path, Pattern: pattern, Reason: reason})
}

// at is the current path as the segments of an issue.
func (v *validation) at() []PathSegment {
	if len(v.path) == 0 {
		return nil
	}
	out := make([]PathSegment, len(v.path))
	for i, s := range v.path {
		if s.index {
			out[i] = s.n
		} else {
			out[i] = s.name
		}
	}
	return out
}

func (v *validation) push(s seg) { v.path = append(v.path, s) }
func (v *validation) pop()       { v.path = v.path[:len(v.path)-1] }

func (v *validation) bad(message string) {
	v.structure = append(v.structure, Issue{Path: v.at(), Message: message})
}

func (v *validation) limit(message string) {
	v.constraints = append(v.constraints, Issue{Path: v.at(), Message: message})
}

func describeValue(value any) string {
	switch value.(type) {
	case nil:
		return "null"
	case bool:
		return "a boolean"
	case int64, float64:
		return "a number"
	case string:
		return "a string"
	case []any:
		return "an array"
	case *Object:
		return "an object"
	}
	return "a value"
}

var (
	formatOnce sync.Once
	formats    map[string]*regexp.Regexp
)

const datePattern = "(?:(?:[0-9][0-9][2468][048]|[0-9][0-9][13579][26]|[0-9][0-9]0[48]|[02468][048]00" +
	"|[13579][26]00)-02-29|[0-9]{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12][0-9]|3[01])" +
	"|(?:0[469]|11)-(?:0[1-9]|[12][0-9]|30)|(?:02)-(?:0[1-9]|1[0-9]|2[0-8])))"

func formatPatterns() map[string]*regexp.Regexp {
	formatOnce.Do(func() {
		ipv4Part := "(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])"
		ipv6 := strings.ReplaceAll("(?:(?:H:){7}H|(?:H:){1,7}:|(?:H:){1,6}:H|(?:H:){1,5}(?::H){1,2}"+
			"|(?:H:){1,4}(?::H){1,3}|(?:H:){1,3}(?::H){1,4}|(?:H:){1,2}(?::H){1,5}"+
			"|H:(?:(?::H){1,6})|:(?:(?::H){1,7}|:))", "H", "[0-9a-fA-F]{1,4}")
		sources := map[string]string{
			"uuid":  "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
			"email": "[^\\s@\"]{1,64}@[^\\s@]{1,255}",
			"date-time": datePattern + "T(?:[01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9](?:\\.[0-9]+)?" +
				"(?:Z|[+-](?:[01][0-9]|2[0-3]):[0-5][0-9])",
			"date": datePattern,
			"ipv4": "(?:" + ipv4Part + "\\.){3}" + ipv4Part,
			"ipv6": ipv6,
		}
		formats = map[string]*regexp.Regexp{}
		for name, src := range sources {
			formats[name] = regexp.MustCompile("^(?:" + src + ")$")
		}
	})
	return formats
}

// compiledPattern is a cached pattern: the expression, or why it cannot be
// checked.
type compiledPattern struct {
	re     *regexp.Regexp
	reason string
}

// patterns caches compiled patterns by source.
var patterns sync.Map

// compiled returns the pattern's expression, or the reason it cannot be
// checked; both are empty without a pattern.
func (s *Schema) compiled() (*regexp.Regexp, string) {
	if s.Pattern == "" {
		return nil, ""
	}
	if cached, ok := patterns.Load(s.Pattern); ok {
		c, _ := cached.(compiledPattern)
		return c.re, c.reason
	}
	// A pattern RE2 cannot compile (lookaround, backreferences) is not
	// checked, as in the Rust runtime; the caller reports it.
	c := compiledPattern{}
	re, err := regexp.Compile(codePointEscapes(s.Pattern))
	if err != nil {
		c.reason = "it is not a regular expression this runtime can compile (Go RE2: " + err.Error() + ")"
	} else {
		c.re = re
	}
	patterns.Store(s.Pattern, c)
	return c.re, c.reason
}

// codePointEscapes rewrites the ECMAScript u-flag escape \u{XXXX} (one to six
// hex digits, at most 10FFFF) to RE2's \x{XXXX}, in and out of classes. An
// escaped backslash, native \Q...\E quoted text and any other escape are copied
// unchanged; a malformed or out-of-range \u{...} is left alone, so it fails to
// compile and is reported as unchecked. Each candidate needs at most seven
// bytes of lookahead (six digits and the closing brace).
func codePointEscapes(pattern string) string {
	if !strings.Contains(pattern, `\u{`) {
		return pattern
	}
	var out strings.Builder
	quoted := false
	for i := 0; i < len(pattern); i++ {
		if quoted {
			out.WriteByte(pattern[i])
			if pattern[i] == '\\' && i+1 < len(pattern) && pattern[i+1] == 'E' {
				out.WriteByte('E')
				i++
				quoted = false
			}
			continue
		}
		if pattern[i] != '\\' || i+1 >= len(pattern) {
			out.WriteByte(pattern[i])
			continue
		}
		if pattern[i+1] == 'Q' {
			quoted = true
		} else if pattern[i+1] == 'u' && i+2 < len(pattern) && pattern[i+2] == '{' {
			start := i + 3
			end := start
			for end < len(pattern) && end-start <= 6 && pattern[end] != '}' {
				end++
			}
			if end-start >= 1 && end-start <= 6 && end < len(pattern) && pattern[end] == '}' {
				digits := pattern[start:end]
				if n, err := strconv.ParseUint(digits, 16, 32); err == nil && n <= 0x10FFFF {
					out.WriteString(`\x{` + digits + `}`)
					i = end
					continue
				}
			}
		}
		out.WriteString(pattern[i : i+2])
		i++
	}
	return out.String()
}

func boundText(f float64) string {
	if f == math.Trunc(f) && math.Abs(f) < maxSafe {
		return strconv.FormatInt(int64(f), 10)
	}
	return JSNumber(f)
}

func (v *validation) check(s *Schema, value any) {
	if s == nil {
		return
	}
	switch s.Kind {
	case "", "any":
	case "never":
		v.bad("no value is allowed here")
	case "nullable":
		if value != nil {
			v.check(s.Inner, value)
		}
	case "string":
		text, ok := value.(string)
		if !ok {
			v.bad("expected a string, found " + describeValue(value))
			return
		}
		v.stringLimits(s, text)
	case "binary":
		if _, ok := BinaryOf(value); ok {
			return
		}
		if _, ok := value.(string); !ok {
			v.bad("expected binary data, found " + describeValue(value))
		}
	case "boolean":
		if _, ok := value.(bool); !ok {
			v.bad("expected a boolean, found " + describeValue(value))
		}
	case "integer":
		n, ok := value.(int64)
		if !ok {
			if f, isFloat := value.(float64); isFloat {
				v.bad("expected an integer, found " + JSNumber(f))
			} else {
				v.bad("expected an integer, found " + describeValue(value))
			}
			return
		}
		switch s.Bits {
		case 32:
			if n < math.MinInt32 || n > math.MaxInt32 {
				v.bad("expected a 32-bit integer")
				return
			}
		}
		v.numberLimits(s, float64(n))
	case "number":
		f, ok := asFloat(value)
		if !ok {
			v.bad("expected a number, found " + describeValue(value))
			return
		}
		v.numberLimits(s, f)
	case "enum":
		for _, allowed := range s.Values {
			if deepEqual(allowed, value) {
				return
			}
		}
		v.bad("must be one of " + JSONText(s.Values))
	case "const":
		if len(s.Values) > 0 && !deepEqual(s.Values[0], value) {
			v.bad("must be " + JSONText(s.Values[0]))
		}
	case "array":
		list, ok := value.([]any)
		if !ok {
			v.bad("expected an array, found " + describeValue(value))
			return
		}
		for i, item := range list {
			v.push(seg{n: i, index: true})
			v.check(s.Items, item)
			v.pop()
		}
		n := int64(len(list))
		if s.MinItems != nil && n < *s.MinItems {
			v.limit(fmt.Sprintf("must have at least %d items", *s.MinItems))
		}
		if s.MaxItems != nil && n > *s.MaxItems {
			v.limit(fmt.Sprintf("must have at most %d items", *s.MaxItems))
		}
		if s.Unique {
			for i := range list {
				for j := 0; j < i; j++ {
					if deepEqual(list[i], list[j]) {
						v.limit("items must be unique")
						return
					}
				}
			}
		}
	case "map":
		obj, ok := value.(*Object)
		if !ok {
			v.bad("expected an object, found " + describeValue(value))
			return
		}
		for _, k := range obj.keys {
			v.push(seg{name: k})
			v.check(s.Items, obj.vals[k])
			v.pop()
		}
	case "object":
		obj, ok := value.(*Object)
		if !ok {
			v.bad("expected an object, found " + describeValue(value))
			return
		}
		v.object(s, obj)
	case "union":
		v.union(s, value)
	case "all":
		for _, member := range s.Variants {
			v.check(member, value)
		}
	case "limits":
		// Constraints written on a field, next to its type: they apply to the
		// value's own kind.
		switch t := value.(type) {
		case string:
			v.stringLimits(s, t)
		case int64, float64:
			f, _ := asFloat(t)
			v.numberLimits(s, f)
		}
	default:
		v.bad("unknown schema kind " + s.Kind)
	}
}

func (v *validation) stringLimits(s *Schema, text string) {
	if s.MinLength != nil || s.MaxLength != nil {
		n := int64(utf8.RuneCountInString(text))
		if s.MinLength != nil && n < *s.MinLength {
			v.limit(fmt.Sprintf("must have at least %d characters", *s.MinLength))
		}
		if s.MaxLength != nil && n > *s.MaxLength {
			v.limit(fmt.Sprintf("must have at most %d characters", *s.MaxLength))
		}
	}
	if re, reason := s.compiled(); re != nil {
		if !re.MatchString(text) {
			v.limit("must match the pattern /" + s.Pattern + "/")
		}
	} else if reason != "" {
		v.unchecked(s.Pattern, reason)
	}
	if s.Format != "" {
		if re, ok := formatPatterns()[s.Format]; ok && !re.MatchString(text) {
			v.limit("must be a valid " + s.Format)
		}
	}
}

func (v *validation) numberLimits(s *Schema, f float64) {
	if s.Minimum != nil && f < *s.Minimum {
		v.limit("must be at least " + boundText(*s.Minimum))
	}
	if s.Maximum != nil && f > *s.Maximum {
		v.limit("must be at most " + boundText(*s.Maximum))
	}
	if s.ExclusiveMinimum != nil && f <= *s.ExclusiveMinimum {
		v.limit("must be greater than " + boundText(*s.ExclusiveMinimum))
	}
	if s.ExclusiveMaximum != nil && f >= *s.ExclusiveMaximum {
		v.limit("must be less than " + boundText(*s.ExclusiveMaximum))
	}
	if s.MultipleOf != nil && *s.MultipleOf != 0 {
		q := f / *s.MultipleOf
		if math.Abs(q-math.Round(q)) > 1e-9*math.Max(math.Abs(q), 1) {
			v.limit("must be a multiple of " + boundText(*s.MultipleOf))
		}
	}
}

// knownField reports whether the object schema declares the member.
func knownField(s *Schema, name string) bool {
	for i := range s.Fields {
		if s.Fields[i].Name == name {
			return true
		}
	}
	return false
}

func (v *validation) object(s *Schema, obj *Object) {
	for i := range s.Fields {
		f := &s.Fields[i]
		value, present := obj.Get(f.Name)
		v.push(seg{name: f.Name})
		switch {
		case !present:
			if f.Required {
				v.bad("missing required member " + f.Name)
			}
		case value == nil && f.Nullable:
		default:
			v.check(f.Schema, value)
		}
		v.pop()
	}
	if s.Extra == nil && s.Additional != "closed" {
		return
	}
	for _, k := range obj.keys {
		if knownField(s, k) {
			continue
		}
		v.push(seg{name: k})
		switch {
		case s.Extra != nil:
			v.check(s.Extra, obj.vals[k])
		case s.Additional == "closed":
			v.unknown = append(v.unknown, Issue{Path: v.at(), Message: "unknown member " + k})
		}
		v.pop()
	}
}

// accepts reports whether the schema admits the value without issues.
func accepts(s *Schema, value any) bool {
	return len(s.Validate(value)) == 0
}

func (v *validation) union(s *Schema, value any) {
	if s.Tag != "" {
		obj, ok := value.(*Object)
		if !ok {
			v.bad("expected an object, found " + describeValue(value))
			return
		}
		tag, present := obj.Get(s.Tag)
		text, isText := tag.(string)
		if !present || !isText {
			v.push(seg{name: s.Tag})
			v.bad("missing the discriminator " + s.Tag)
			v.pop()
			return
		}
		for i, t := range s.Tags {
			if t == text && i < len(s.Variants) {
				v.check(s.Variants[i], value)
				return
			}
		}
		v.push(seg{name: s.Tag})
		v.bad("must be one of " + JSONText(stringsToAny(s.Tags)))
		v.pop()
		return
	}
	for _, variant := range s.Variants {
		if accepts(variant, value) {
			return
		}
	}
	v.bad("does not match any variant")
}

func stringsToAny(list []string) []any {
	out := make([]any, len(list))
	for i, s := range list {
		out[i] = s
	}
	return out
}

// Int64 is a pointer to an int64, for schema bounds.
func Int64(n int64) *int64 { return &n }

// Float is a pointer to a float64, for schema bounds.
func Float(f float64) *float64 { return &f }

// formatIssuePath writes an issue path as .key and [index] segments.
func formatIssuePath(path []PathSegment) string {
	var b strings.Builder
	for _, seg := range path {
		switch t := seg.(type) {
		case int:
			b.WriteString("[" + strconv.Itoa(t) + "]")
		case string:
			if isIndex(t) {
				b.WriteString("[" + t + "]")
			} else {
				b.WriteString("." + t)
			}
		}
	}
	return b.String()
}

func segmentString(seg PathSegment) string {
	switch t := seg.(type) {
	case int:
		return strconv.Itoa(t)
	case string:
		return t
	}
	return ""
}

func segmentStrings(path []PathSegment) []string {
	out := make([]string, len(path))
	for i, seg := range path {
		out[i] = segmentString(seg)
	}
	return out
}
