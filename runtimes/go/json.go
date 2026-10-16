// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"reflect"
	"sort"
	"strconv"
	"strings"
	"unicode/utf16"
	"unicode/utf8"
)

// JSON values in this package are plain Go values: nil (null), bool,
// string, int64 or float64 (numbers), []any (arrays) and *Object (objects,
// which keep the order of their members). ParseJSON produces them; FromGo
// converts other Go values (maps, structs, integers) to them.

// Object is a JSON object whose members keep their insertion order, so
// request bodies, digests and texts are the same in every runtime.
type Object struct {
	keys []string
	vals map[string]any
}

// NewObject returns an empty object.
func NewObject() *Object {
	return &Object{vals: map[string]any{}}
}

// Len is the number of members.
func (o *Object) Len() int {
	if o == nil {
		return 0
	}
	return len(o.keys)
}

// Keys returns the member names in order.
func (o *Object) Keys() []string {
	if o == nil {
		return nil
	}
	out := make([]string, len(o.keys))
	copy(out, o.keys)
	return out
}

// Get returns a member and whether it is present.
func (o *Object) Get(key string) (any, bool) {
	if o == nil {
		return nil, false
	}
	v, ok := o.vals[key]
	return v, ok
}

// Has reports whether the member is present.
func (o *Object) Has(key string) bool {
	_, ok := o.Get(key)
	return ok
}

// Set adds a member at the end, or replaces the value of an existing one in
// its place.
func (o *Object) Set(key string, value any) {
	if o.vals == nil {
		o.vals = map[string]any{}
	}
	if _, ok := o.vals[key]; !ok {
		o.keys = append(o.keys, key)
	}
	o.vals[key] = value
}

// Delete removes a member.
func (o *Object) Delete(key string) {
	if o == nil {
		return
	}
	if _, ok := o.vals[key]; !ok {
		return
	}
	delete(o.vals, key)
	for i, k := range o.keys {
		if k == key {
			o.keys = append(o.keys[:i:i], o.keys[i+1:]...)
			break
		}
	}
}

// Clone returns a shallow copy.
func (o *Object) Clone() *Object {
	out := &Object{keys: make([]string, 0, o.Len()), vals: make(map[string]any, o.Len())}
	if o == nil {
		return out
	}
	for _, k := range o.keys {
		out.keys = append(out.keys, k)
		out.vals[k] = o.vals[k]
	}
	return out
}

// MarshalJSON writes the object as JSONText does.
func (o *Object) MarshalJSON() ([]byte, error) {
	return []byte(JSONText(o)), nil
}

// UnmarshalJSON reads an object, keeping member order.
func (o *Object) UnmarshalJSON(data []byte) error {
	v, err := ParseJSON(data)
	if err != nil {
		return err
	}
	obj, ok := v.(*Object)
	if !ok {
		return errors.New("tungsten: not a JSON object")
	}
	*o = *obj
	return nil
}

// DeepCopy copies a JSON value.
func DeepCopy(v any) any {
	switch t := v.(type) {
	case []any:
		out := make([]any, len(t))
		for i, item := range t {
			out[i] = DeepCopy(item)
		}
		return out
	case *Object:
		out := &Object{keys: make([]string, 0, t.Len()), vals: make(map[string]any, t.Len())}
		for _, k := range t.keys {
			out.keys = append(out.keys, k)
			out.vals[k] = DeepCopy(t.vals[k])
		}
		return out
	default:
		return v
	}
}

// maxSafe is 2^53, the bound of the integers a JavaScript number holds.
const maxSafe = 9007199254740992.0

// parseInfo is what the parser noticed about the text beyond the value it
// read: whether the text is the exact JSONText of the value, and whether a
// negative zero was read (the values of this package have no negative
// zero). Both let a typed decode read the original bytes instead of a
// re-serialized value.
type parseInfo struct {
	// verbatim is true when the text is exactly JSONText(value): no
	// whitespace, canonical escapes and number texts, no repeated member.
	verbatim bool
	negZero  bool
	// changedValue requires decoding the normalized value even for ordinary types.
	changedValue bool
}

// ParseJSON parses one JSON document. Object members keep their order;
// integral numbers below 2^53 in magnitude are int64, as the other runtimes
// read them; other numbers are float64.
func ParseJSON(data []byte) (any, error) {
	v, _, err := parseJSON(data, utf8.Valid(data))
	return v, err
}

// ParseJSONString parses JSON text.
func ParseJSONString(text string) (any, error) {
	return ParseJSON([]byte(text))
}

const maxParseDepth = 10000

// parseJSON parses data; validUTF8 says the caller already knows data is
// valid UTF-8 (invalid bytes inside strings are otherwise replaced with
// U+FFFD, as encoding/json does). The text is copied once: member names
// and string values without escapes are substrings of that copy.
func parseJSON(data []byte, validUTF8 bool) (any, parseInfo, error) {
	p := parser{text: string(data), validUTF8: validUTF8, info: parseInfo{verbatim: true}}
	v, err := p.value(0)
	if err != nil {
		return nil, parseInfo{}, err
	}
	// Anything left that is not whitespace is trailing data.
	p.skipSpace()
	if p.pos != len(p.text) {
		return nil, parseInfo{}, errors.New("tungsten: trailing data after the JSON value")
	}
	return v, p.info, nil
}

// parser is a single-pass reader of JSON text into the values of this
// package. Members and items are gathered on shared stacks so each object
// and array is allocated once, at its exact size, carved from slabs the
// parser fills as it goes.
type parser struct {
	text      string
	pos       int
	validUTF8 bool
	info      parseInfo
	// keys and vals are the stacks of the members and items read so far.
	keys []string
	vals []any
	// objs, keySlab and anySlab are the slabs objects, their member names
	// and array items are carved from (each slice capped at its length, so
	// a later append to it copies instead of touching its neighbour).
	objs    []Object
	keySlab []string
	anySlab []any
	scratch []byte
	// slab is the size of the next slab: small for a small document,
	// doubling up to maxSlab.
	slab int
}

const (
	minSlab = 8
	maxSlab = 64
)

func (p *parser) slabSize() int {
	if p.slab == 0 {
		p.slab = minSlab
	}
	n := p.slab
	if p.slab < maxSlab {
		p.slab *= 2
	}
	return n
}

func (p *parser) newObject(n int) *Object {
	if len(p.objs) == 0 {
		p.objs = make([]Object, p.slabSize())
	}
	o := &p.objs[0]
	p.objs = p.objs[1:]
	if n == 0 {
		o.vals = map[string]any{}
		return o
	}
	if n > maxSlab/2 {
		o.keys = make([]string, 0, n)
	} else {
		if len(p.keySlab) < n {
			p.keySlab = make([]string, max(n, p.slabSize()))
		}
		o.keys = p.keySlab[:0:n]
		p.keySlab = p.keySlab[n:]
	}
	o.vals = make(map[string]any, n)
	return o
}

func (p *parser) newList(n int) []any {
	if n == 0 {
		return []any{}
	}
	if n > maxSlab/2 {
		return make([]any, n)
	}
	if len(p.anySlab) < n {
		p.anySlab = make([]any, max(n, p.slabSize()))
	}
	list := p.anySlab[:n:n]
	p.anySlab = p.anySlab[n:]
	return list
}

func (p *parser) skipSpace() {
	for p.pos < len(p.text) {
		switch p.text[p.pos] {
		case ' ', '\t', '\r', '\n':
			p.pos++
			p.info.verbatim = false
		default:
			return
		}
	}
}

func (p *parser) fail(what string) error {
	if p.pos >= len(p.text) {
		return errors.New("tungsten: unexpected end of JSON input")
	}
	return fmt.Errorf("tungsten: invalid character %q %s at offset %d", p.text[p.pos], what, p.pos)
}

func (p *parser) value(depth int) (any, error) {
	if depth > maxParseDepth {
		return nil, errors.New("tungsten: JSON nested too deeply")
	}
	p.skipSpace()
	if p.pos >= len(p.text) {
		return nil, p.fail("")
	}
	switch c := p.text[p.pos]; c {
	case '{':
		return p.object(depth)
	case '[':
		return p.array(depth)
	case '"':
		return p.str()
	case 't':
		return p.literal("true", true)
	case 'f':
		return p.literal("false", false)
	case 'n':
		return p.literal("null", nil)
	default:
		if c == '-' || (c >= '0' && c <= '9') {
			return p.number()
		}
		return nil, p.fail("looking for beginning of value")
	}
}

func (p *parser) literal(text string, v any) (any, error) {
	if strings.HasPrefix(p.text[p.pos:], text) {
		p.pos += len(text)
		return v, nil
	}
	for i := 0; i < len(text) && p.pos+i < len(p.text); i++ {
		if p.text[p.pos+i] != text[i] {
			p.pos += i
			break
		}
	}
	return nil, p.fail("in literal")
}

func (p *parser) object(depth int) (any, error) {
	p.pos++ // {
	p.skipSpace()
	if p.pos < len(p.text) && p.text[p.pos] == '}' {
		p.pos++
		return p.newObject(0), nil
	}
	keyMark, valMark := len(p.keys), len(p.vals)
	for {
		p.skipSpace()
		if p.pos >= len(p.text) || p.text[p.pos] != '"' {
			return nil, p.fail("looking for beginning of object key string")
		}
		key, err := p.str()
		if err != nil {
			return nil, err
		}
		p.skipSpace()
		if p.pos >= len(p.text) || p.text[p.pos] != ':' {
			return nil, p.fail("after object key")
		}
		p.pos++
		val, err := p.value(depth + 1)
		if err != nil {
			return nil, err
		}
		p.keys = append(p.keys, key)
		p.vals = append(p.vals, val)
		p.skipSpace()
		if p.pos >= len(p.text) {
			return nil, p.fail("")
		}
		if c := p.text[p.pos]; c == ',' {
			p.pos++
			continue
		} else if c == '}' {
			p.pos++
			break
		}
		return nil, p.fail("after object key:value pair")
	}
	n := len(p.keys) - keyMark
	obj := p.newObject(n)
	for i := 0; i < n; i++ {
		k := p.keys[keyMark+i]
		if _, dup := obj.vals[k]; dup {
			// A repeated member keeps its first place and its last value.
			p.info.verbatim = false
			p.info.changedValue = true
		} else {
			obj.keys = append(obj.keys, k)
		}
		obj.vals[k] = p.vals[valMark+i]
	}
	clear(p.keys[keyMark:])
	clear(p.vals[valMark:])
	p.keys, p.vals = p.keys[:keyMark], p.vals[:valMark]
	return obj, nil
}

func (p *parser) array(depth int) (any, error) {
	p.pos++ // [
	p.skipSpace()
	if p.pos < len(p.text) && p.text[p.pos] == ']' {
		p.pos++
		return p.newList(0), nil
	}
	valMark := len(p.vals)
	for {
		val, err := p.value(depth + 1)
		if err != nil {
			return nil, err
		}
		p.vals = append(p.vals, val)
		p.skipSpace()
		if p.pos >= len(p.text) {
			return nil, p.fail("")
		}
		if c := p.text[p.pos]; c == ',' {
			p.pos++
			continue
		} else if c == ']' {
			p.pos++
			break
		}
		return nil, p.fail("after array element")
	}
	list := p.newList(len(p.vals) - valMark)
	copy(list, p.vals[valMark:])
	clear(p.vals[valMark:])
	p.vals = p.vals[:valMark]
	return list, nil
}

// str reads a string literal at p.pos (a double quote). A literal without
// escapes is a substring of the text.
func (p *parser) str() (string, error) {
	start := p.pos + 1
	i := start
	ascii := true
	for i < len(p.text) {
		c := p.text[i]
		switch {
		case c == '"':
			p.pos = i + 1
			if ascii || p.validUTF8 || utf8.ValidString(p.text[start:i]) {
				return p.text[start:i], nil
			}
			// Invalid bytes are replaced one by one, as encoding/json does.
			return p.escapedString(start)
		case c == '\\':
			return p.escapedString(start)
		case c < 0x20:
			p.pos = i
			return "", p.fail("in string literal")
		case c >= utf8.RuneSelf:
			ascii = false
		}
		i++
	}
	p.pos = len(p.text)
	return "", p.fail("")
}

// escapedString reads a string literal with escapes or invalid UTF-8 byte
// by byte; start is the first byte after the opening quote.
func (p *parser) escapedString(start int) (string, error) {
	buf := p.scratch[:0]
	i := start
	for i < len(p.text) {
		c := p.text[i]
		switch {
		case c == '"':
			p.pos = i + 1
			p.scratch = buf[:0]
			return string(buf), nil
		case c == '\\':
			if i+1 >= len(p.text) {
				p.pos = len(p.text)
				return "", p.fail("")
			}
			i++
			switch p.text[i] {
			case '"':
				buf = append(buf, '"')
			case '\\':
				buf = append(buf, '\\')
			case '/':
				buf = append(buf, '/')
				p.info.verbatim = false
			case 'b':
				buf = append(buf, '\b')
			case 'f':
				buf = append(buf, '\f')
			case 'n':
				buf = append(buf, '\n')
			case 'r':
				buf = append(buf, '\r')
			case 't':
				buf = append(buf, '\t')
			case 'u':
				r, ok := hex4(p.text, i+1)
				if !ok {
					p.pos = i + 1
					return "", p.fail("in \\u hexadecimal character escape")
				}
				i += 4
				// JSONText writes \u00xx (lower case) only for the control
				// characters without a short escape.
				switch r {
				case '\b', '\t', '\n', '\f', '\r':
					p.info.verbatim = false
				default:
					if r >= 0x20 || p.text[i-1]|0x20 != p.text[i-1] || p.text[i]|0x20 != p.text[i] {
						p.info.verbatim = false
					}
				}
				if utf16.IsSurrogate(r) {
					var r2 rune
					ok := false
					if i+6 < len(p.text) && p.text[i+1] == '\\' && p.text[i+2] == 'u' {
						r2, ok = hex4(p.text, i+3)
					}
					if dec := utf16.DecodeRune(r, r2); ok && dec != utf8.RuneError {
						r = dec
						i += 6
					} else {
						r = utf8.RuneError
					}
				}
				buf = utf8.AppendRune(buf, r)
			default:
				p.pos = i
				return "", p.fail("in string escape code")
			}
			i++
		case c < 0x20:
			p.pos = i
			return "", p.fail("in string literal")
		case c < utf8.RuneSelf:
			buf = append(buf, c)
			i++
		default:
			r, size := utf8.DecodeRuneInString(p.text[i:])
			if r == utf8.RuneError && size == 1 {
				buf = utf8.AppendRune(buf, utf8.RuneError)
				p.info.verbatim = false
			} else {
				buf = append(buf, p.text[i:i+size]...)
			}
			i += size
		}
	}
	p.pos = len(p.text)
	return "", p.fail("")
}

// hex4 reads four hexadecimal digits at text[at:].
func hex4(text string, at int) (rune, bool) {
	if at+4 > len(text) {
		return 0, false
	}
	var r rune
	for i := at; i < at+4; i++ {
		c := text[i]
		switch {
		case c >= '0' && c <= '9':
			c -= '0'
		case c >= 'a' && c <= 'f':
			c = c - 'a' + 10
		case c >= 'A' && c <= 'F':
			c = c - 'A' + 10
		default:
			return 0, false
		}
		r = r<<4 | rune(c)
	}
	return r, true
}

func (p *parser) number() (any, error) {
	start := p.pos
	i := start
	text := p.text
	if text[i] == '-' {
		i++
	}
	digits := func() bool {
		if i >= len(text) || text[i] < '0' || text[i] > '9' {
			return false
		}
		for i < len(text) && text[i] >= '0' && text[i] <= '9' {
			i++
		}
		return true
	}
	if i < len(text) && text[i] == '0' {
		i++
	} else if !digits() {
		p.pos = i
		return nil, p.fail("in numeric literal")
	}
	integral := true
	if i < len(text) && text[i] == '.' {
		integral = false
		i++
		if !digits() {
			p.pos = i
			return nil, p.fail("after decimal point in numeric literal")
		}
	}
	if i < len(text) && (text[i] == 'e' || text[i] == 'E') {
		integral = false
		i++
		if i < len(text) && (text[i] == '+' || text[i] == '-') {
			i++
		}
		if !digits() {
			p.pos = i
			return nil, p.fail("in exponent of numeric literal")
		}
	}
	p.pos = i
	lit := text[start:i]
	if integral {
		if n, err := strconv.ParseInt(lit, 10, 64); err == nil {
			if n == 0 && lit[0] == '-' {
				p.info.negZero, p.info.verbatim = true, false
			}
			return n, nil
		}
		// Integers outside int64 are rounded through float64 in Raw.
		p.info.changedValue = true
	}
	v, err := parseNumber(lit)
	if err != nil {
		return nil, err
	}
	if lit[0] == '-' {
		if f, isFloat := v.(float64); (isFloat && f == 0) || v == any(int64(0)) {
			p.info.negZero = true
		}
	}
	if p.info.verbatim {
		switch t := v.(type) {
		case int64:
			p.info.verbatim = string(strconv.AppendInt(p.scratch[:0], t, 10)) == lit
		case float64:
			p.scratch = appendJSNumber(p.scratch[:0], t)
			p.info.verbatim = string(p.scratch) == lit
			p.scratch = p.scratch[:0]
		}
	}
	return v, nil
}

func parseNumber(text string) (any, error) {
	if !strings.ContainsAny(text, ".eE") {
		if i, err := strconv.ParseInt(text, 10, 64); err == nil {
			return i, nil
		}
	}
	f, err := strconv.ParseFloat(text, 64)
	if err != nil {
		return nil, fmt.Errorf("tungsten: number %s out of range", text)
	}
	return normalizeFloat(f), nil
}

func normalizeFloat(f float64) any {
	if f == math.Trunc(f) && math.Abs(f) < maxSafe {
		return int64(f)
	}
	return f
}

// FromGo converts a Go value to the JSON values of this package: maps
// (members sorted by name), slices, structs (through encoding/json),
// integers and floats. JSON values pass through unchanged (objects and
// arrays are copied).
func FromGo(v any) (any, error) {
	switch t := v.(type) {
	case nil:
		return nil, nil
	case bool, string, int64:
		return t, nil
	case float64:
		if math.IsNaN(t) || math.IsInf(t, 0) {
			return nil, errors.New("tungsten: a non-finite number is not JSON")
		}
		return normalizeFloat(t), nil
	case int:
		return int64(t), nil
	case int32:
		return int64(t), nil
	case float32:
		return normalizeFloat(float64(t)), nil
	case json.Number:
		return parseNumber(string(t))
	case json.RawMessage:
		return ParseJSON(t)
	case *Object:
		out := NewObject()
		for _, k := range t.keys {
			c, err := FromGo(t.vals[k])
			if err != nil {
				return nil, err
			}
			out.Set(k, c)
		}
		return out, nil
	case []any:
		out := make([]any, len(t))
		for i, item := range t {
			c, err := FromGo(item)
			if err != nil {
				return nil, err
			}
			out[i] = c
		}
		return out, nil
	case map[string]any:
		keys := make([]string, 0, len(t))
		for k := range t {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		out := NewObject()
		for _, k := range keys {
			c, err := FromGo(t[k])
			if err != nil {
				return nil, err
			}
			out.Set(k, c)
		}
		return out, nil
	case map[string]string:
		keys := make([]string, 0, len(t))
		for k := range t {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		out := NewObject()
		for _, k := range keys {
			out.Set(k, t[k])
		}
		return out, nil
	case []string:
		out := make([]any, len(t))
		for i, s := range t {
			out[i] = s
		}
		return out, nil
	}
	rv := reflect.ValueOf(v)
	if rv.Kind() == reflect.Pointer && rv.IsNil() {
		return nil, nil
	}
	data, err := json.Marshal(v)
	if err != nil {
		return nil, err
	}
	return ParseJSON(data)
}

// Marshal encodes a Go value (a generated request struct, a model) as a JSON
// value of this package.
func Marshal(v any) (any, error) {
	return FromGo(v)
}

// Unmarshal decodes a JSON value of this package into a Go value through
// encoding/json.
func Unmarshal(value any, out any) error {
	return json.Unmarshal([]byte(JSONText(value)), out)
}

// ------------------------------------------------------- JSON text forms

const hexDigits = "0123456789abcdef"

func writeJSString(b *strings.Builder, text string) {
	b.WriteByte('"')
	for _, r := range text {
		switch r {
		case '"':
			b.WriteString(`\"`)
		case '\\':
			b.WriteString(`\\`)
		case '\b':
			b.WriteString(`\b`)
		case '\f':
			b.WriteString(`\f`)
		case '\n':
			b.WriteString(`\n`)
		case '\r':
			b.WriteString(`\r`)
		case '\t':
			b.WriteString(`\t`)
		default:
			if r < 0x20 {
				b.WriteString(`\u00`)
				b.WriteByte(hexDigits[r>>4])
				b.WriteByte(hexDigits[r&15])
			} else {
				b.WriteRune(r)
			}
		}
	}
	b.WriteByte('"')
}

// jsString is a string literal as JSON.stringify writes it.
func jsString(text string) string {
	var b strings.Builder
	writeJSString(&b, text)
	return b.String()
}

// JSNumber is String(value) for a JavaScript number (ECMAScript
// Number::toString): 1.0 is "1", 1e-7 is "1e-7", 1e21 is "1e+21".
func JSNumber(value float64) string {
	return string(appendJSNumber(nil, value))
}

// appendJSNumber appends JSNumber(value) to dst.
func appendJSNumber(dst []byte, value float64) []byte {
	if math.IsNaN(value) {
		return append(dst, "NaN"...)
	}
	if math.IsInf(value, 1) {
		return append(dst, "Infinity"...)
	}
	if math.IsInf(value, -1) {
		return append(dst, "-Infinity"...)
	}
	if value == 0 {
		return append(dst, '0')
	}
	if value < 0 {
		dst = append(dst, '-')
		value = -value
	}
	var tmp [32]byte
	sci := strconv.AppendFloat(tmp[:0], value, 'e', -1, 64)
	e := 0
	for sci[e] != 'e' {
		e++
	}
	// The exponent: a sign and digits.
	exponent := 0
	for _, c := range sci[e+2:] {
		exponent = exponent*10 + int(c-'0')
	}
	if sci[e+1] == '-' {
		exponent = -exponent
	}
	// The significant digits, without the point or trailing zeros.
	var dbuf [24]byte
	digits := dbuf[:0]
	for _, c := range sci[:e] {
		if c != '.' {
			digits = append(digits, c)
		}
	}
	for len(digits) > 0 && digits[len(digits)-1] == '0' {
		digits = digits[:len(digits)-1]
	}
	if len(digits) == 0 {
		digits = append(digits, '0')
	}
	k := len(digits)
	n := exponent + 1
	switch {
	case k <= n && n <= 21:
		dst = append(dst, digits...)
		for i := 0; i < n-k; i++ {
			dst = append(dst, '0')
		}
	case 0 < n && n <= 21:
		dst = append(dst, digits[:n]...)
		dst = append(dst, '.')
		dst = append(dst, digits[n:]...)
	case -6 < n && n <= 0:
		dst = append(dst, '0', '.')
		for i := 0; i < -n; i++ {
			dst = append(dst, '0')
		}
		dst = append(dst, digits...)
	default:
		exp := n - 1
		mark := byte('+')
		if exp < 0 {
			mark = '-'
			exp = -exp
		}
		dst = append(dst, digits[0])
		if k > 1 {
			dst = append(dst, '.')
			dst = append(dst, digits[1:]...)
		}
		dst = append(dst, 'e', mark)
		dst = strconv.AppendInt(dst, int64(exp), 10)
	}
	return dst
}

// numberText is a JSON number as JavaScript writes it.
func numberText(v any) string {
	switch t := v.(type) {
	case int64:
		return strconv.FormatInt(t, 10)
	case float64:
		if math.IsNaN(t) || math.IsInf(t, 0) {
			return "null"
		}
		return JSNumber(t)
	}
	return "null"
}

// JSONText is JSON.stringify(value): members in insertion order, no
// whitespace, numbers as JavaScript writes them.
func JSONText(v any) string {
	var b strings.Builder
	writeJSON(&b, v, false)
	return b.String()
}

// CanonicalJSON is the canonical JSON every tungsten runtime digests
// (confirmation tokens, automatic idempotency keys, content hashes): object
// members sorted by the UTF-16 code units of their names, no whitespace,
// numbers as JavaScript writes them, binary values as {"$bytes": <base64>}
// (or {"$file": {"bytes", "name", "type"}} with a name or type).
func CanonicalJSON(v any) string {
	var b strings.Builder
	writeJSON(&b, v, true)
	return b.String()
}

func utf16Less(a, b string) bool {
	ua, ub := utf16.Encode([]rune(a)), utf16.Encode([]rune(b))
	for i := 0; i < len(ua) && i < len(ub); i++ {
		if ua[i] != ub[i] {
			return ua[i] < ub[i]
		}
	}
	return len(ua) < len(ub)
}

func writeJSON(b *strings.Builder, v any, canonical bool) {
	switch t := v.(type) {
	case nil:
		b.WriteString("null")
	case bool:
		if t {
			b.WriteString("true")
		} else {
			b.WriteString("false")
		}
	case int64, float64:
		b.WriteString(numberText(t))
	case string:
		writeJSString(b, t)
	case []any:
		b.WriteByte('[')
		for i, item := range t {
			if i > 0 {
				b.WriteByte(',')
			}
			writeJSON(b, item, canonical)
		}
		b.WriteByte(']')
	case *Object:
		if canonical {
			if bin, ok := BinaryOf(t); ok {
				writeBinary(b, bin)
				return
			}
		}
		keys := t.Keys()
		if canonical {
			sort.SliceStable(keys, func(i, j int) bool { return utf16Less(keys[i], keys[j]) })
		}
		b.WriteByte('{')
		for i, k := range keys {
			if i > 0 {
				b.WriteByte(',')
			}
			writeJSString(b, k)
			b.WriteByte(':')
			writeJSON(b, t.vals[k], canonical)
		}
		b.WriteByte('}')
	default:
		// Other Go values (structs, maps, sized integers) are converted
		// first; FromGo only returns the value kinds above.
		converted, err := FromGo(v)
		if err != nil {
			b.WriteString("null")
			return
		}
		writeJSON(b, converted, canonical)
	}
}

func writeBinary(b *strings.Builder, bin Binary) {
	encoded := base64Standard(bin.Data)
	if bin.Filename == "" && bin.ContentType == "" {
		b.WriteString(`{"$bytes":`)
		writeJSString(b, encoded)
		b.WriteByte('}')
		return
	}
	b.WriteString(`{"$file":{"bytes":`)
	writeJSString(b, encoded)
	b.WriteString(`,"name":`)
	if bin.Filename != "" {
		writeJSString(b, bin.Filename)
	} else {
		b.WriteString("null")
	}
	b.WriteString(`,"type":`)
	if bin.ContentType != "" {
		writeJSString(b, bin.ContentType)
	} else {
		b.WriteString("null")
	}
	b.WriteString("}}")
}

// utf16Len is the length of a text in UTF-16 code units, the unit
// JavaScript counts in.
func utf16Len(text string) int {
	n := 0
	for _, r := range text {
		if r >= 0x10000 {
			n += 2
		} else {
			n++
		}
	}
	return n
}

// takeUnits is the first max UTF-16 code units of text, never splitting a
// character.
func takeUnits(text string, max int) string {
	used := 0
	for i, r := range text {
		w := 1
		if r >= 0x10000 {
			w = 2
		}
		if used+w > max {
			return text[:i]
		}
		used += w
	}
	return text
}

// validUTF8 replaces invalid UTF-8 with U+FFFD.
func validUTF8(data []byte) string {
	if utf8.Valid(data) {
		return string(data)
	}
	return strings.ToValidUTF8(string(data), "\uFFFD")
}
