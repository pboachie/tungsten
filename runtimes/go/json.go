// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"bytes"
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

// ParseJSON parses one JSON document. Object members keep their order;
// integral numbers below 2^53 in magnitude are int64, as the other runtimes
// read them; other numbers are float64.
func ParseJSON(data []byte) (any, error) {
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.UseNumber()
	v, err := parseValue(dec, 0)
	if err != nil {
		return nil, err
	}
	// Anything left that is not whitespace is trailing data.
	rest := data[dec.InputOffset():]
	if len(bytes.Trim(rest, " \t\r\n")) != 0 {
		return nil, errors.New("tungsten: trailing data after the JSON value")
	}
	return v, nil
}

// ParseJSONString parses JSON text.
func ParseJSONString(text string) (any, error) {
	return ParseJSON([]byte(text))
}

const maxParseDepth = 10000

func parseValue(dec *json.Decoder, depth int) (any, error) {
	if depth > maxParseDepth {
		return nil, errors.New("tungsten: JSON nested too deeply")
	}
	tok, err := dec.Token()
	if err != nil {
		return nil, err
	}
	switch t := tok.(type) {
	case json.Delim:
		switch t {
		case '{':
			obj := NewObject()
			for dec.More() {
				keyTok, err := dec.Token()
				if err != nil {
					return nil, err
				}
				key, ok := keyTok.(string)
				if !ok {
					return nil, errors.New("tungsten: object key is not a string")
				}
				val, err := parseValue(dec, depth+1)
				if err != nil {
					return nil, err
				}
				obj.Set(key, val)
			}
			if _, err := dec.Token(); err != nil {
				return nil, err
			}
			return obj, nil
		case '[':
			list := []any{}
			for dec.More() {
				val, err := parseValue(dec, depth+1)
				if err != nil {
					return nil, err
				}
				list = append(list, val)
			}
			if _, err := dec.Token(); err != nil {
				return nil, err
			}
			return list, nil
		default:
			return nil, fmt.Errorf("tungsten: unexpected %v", t)
		}
	case json.Number:
		return parseNumber(string(t))
	case string, bool, nil:
		return t, nil
	default:
		return nil, fmt.Errorf("tungsten: unexpected token %v", tok)
	}
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
	if math.IsNaN(value) {
		return "NaN"
	}
	if math.IsInf(value, 1) {
		return "Infinity"
	}
	if math.IsInf(value, -1) {
		return "-Infinity"
	}
	if value == 0 {
		return "0"
	}
	sign := ""
	if value < 0 {
		sign = "-"
		value = -value
	}
	sci := strconv.FormatFloat(value, 'e', -1, 64)
	mantissa, expText, _ := strings.Cut(sci, "e")
	exponent, _ := strconv.Atoi(expText)
	digits := strings.TrimRight(strings.ReplaceAll(mantissa, ".", ""), "0")
	if digits == "" {
		digits = "0"
	}
	k := len(digits)
	n := exponent + 1
	var body string
	switch {
	case k <= n && n <= 21:
		body = digits + strings.Repeat("0", n-k)
	case 0 < n && n <= 21:
		body = digits[:n] + "." + digits[n:]
	case -6 < n && n <= 0:
		body = "0." + strings.Repeat("0", -n) + digits
	default:
		e := n - 1
		mark := "+"
		if e < 0 {
			mark = "-"
			e = -e
		}
		head := digits
		if k > 1 {
			head = digits[:1] + "." + digits[1:]
		}
		body = head + "e" + mark + strconv.Itoa(e)
	}
	return sign + body
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
