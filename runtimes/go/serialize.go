// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"fmt"
	"strconv"
	"strings"
)

// serializationError becomes VALIDATION_FAILED.
type serializationError struct {
	parameter string
	expected  string
	value     any
}

// scalar is String(value) for a scalar, JSON text for anything else.
func scalar(v any) string {
	switch t := v.(type) {
	case string:
		return t
	case int64, float64:
		return numberText(t)
	case bool:
		return strconv.FormatBool(t)
	}
	return JSONText(v)
}

func isUnreserved(b byte, extra string) bool {
	return (b >= 'a' && b <= 'z') || (b >= 'A' && b <= 'Z') || (b >= '0' && b <= '9') || strings.IndexByte(extra, b) >= 0
}

// encodeComponent is encodeURIComponent.
func encodeComponent(text string) string {
	var b strings.Builder
	for i := 0; i < len(text); i++ {
		c := text[i]
		if isUnreserved(c, "-_.!~*'()") {
			b.WriteByte(c)
		} else {
			fmt.Fprintf(&b, "%%%02X", c)
		}
	}
	return b.String()
}

// encodeFormComponent is one component of a form body (URLSearchParams).
func encodeFormComponent(text string) string {
	var b strings.Builder
	for i := 0; i < len(text); i++ {
		c := text[i]
		switch {
		case isUnreserved(c, "*-._"):
			b.WriteByte(c)
		case c == ' ':
			b.WriteByte('+')
		default:
			fmt.Fprintf(&b, "%%%02X", c)
		}
	}
	return b.String()
}

// percentEncode is RFC 3986 percent-encoding.
func percentEncode(text string) string {
	var b strings.Builder
	for i := 0; i < len(text); i++ {
		c := text[i]
		if isUnreserved(c, "-._~") {
			b.WriteByte(c)
		} else {
			fmt.Fprintf(&b, "%%%02X", c)
		}
	}
	return b.String()
}

type pair struct{ k, v string }

func encodedItems(items []any) []string {
	var out []string
	for _, v := range items {
		if v != nil {
			out = append(out, encodeComponent(scalar(v)))
		}
	}
	return out
}

func encodedPairs(o *Object) []pair {
	var out []pair
	for _, k := range o.keys {
		v := o.vals[k]
		if v != nil {
			out = append(out, pair{encodeComponent(k), encodeComponent(scalar(v))})
		}
	}
	return out
}

func flatPairs(pairs []pair) []string {
	out := make([]string, 0, 2*len(pairs))
	for _, p := range pairs {
		out = append(out, p.k, p.v)
	}
	return out
}

func joinedPairs(pairs []pair) []string {
	out := make([]string, 0, len(pairs))
	for _, p := range pairs {
		out = append(out, p.k+"="+p.v)
	}
	return out
}

// serializePathParam renders one path parameter value, percent-encoded.
func serializePathParam(p *ParamDescriptor, v any) string {
	name := encodeComponent(p.Wire)
	style := StyleSimple
	if p.Style == StyleLabel || p.Style == StyleMatrix {
		style = p.Style
	}
	switch t := v.(type) {
	case []any:
		items := encodedItems(t)
		switch {
		case style == StyleLabel:
			sep := ","
			if p.Explode {
				sep = "."
			}
			return "." + strings.Join(items, sep)
		case style == StyleMatrix && p.Explode:
			var b strings.Builder
			for _, i := range items {
				b.WriteString(";" + name + "=" + i)
			}
			return b.String()
		case style == StyleMatrix:
			return ";" + name + "=" + strings.Join(items, ",")
		}
		return strings.Join(items, ",")
	case *Object:
		pairs := encodedPairs(t)
		exploded := joinedPairs(pairs)
		flat := flatPairs(pairs)
		switch {
		case style == StyleLabel && p.Explode:
			return "." + strings.Join(exploded, ".")
		case style == StyleLabel:
			return "." + strings.Join(flat, ",")
		case style == StyleMatrix && p.Explode:
			var b strings.Builder
			for _, e := range exploded {
				b.WriteString(";" + e)
			}
			return b.String()
		case style == StyleMatrix:
			return ";" + name + "=" + strings.Join(flat, ",")
		case p.Explode:
			return strings.Join(exploded, ",")
		}
		return strings.Join(flat, ",")
	}
	val := encodeComponent(scalar(v))
	switch style {
	case StyleLabel:
		return "." + val
	case StyleMatrix:
		return ";" + name + "=" + val
	}
	return val
}

func deepObjectPairs(prefix string, v any, out *[]string, depth int) {
	if depth > 16 || v == nil {
		return
	}
	switch t := v.(type) {
	case []any:
		for _, item := range t {
			deepObjectPairs(prefix, item, out, depth+1)
		}
	case *Object:
		for _, k := range t.keys {
			if t.vals[k] != nil {
				deepObjectPairs(prefix+"["+encodeComponent(k)+"]", t.vals[k], out, depth+1)
			}
		}
	default:
		*out = append(*out, prefix+"="+encodeComponent(scalar(v)))
	}
}

// serializeQueryParam renders the query parts (name=value, encoded) of one
// parameter.
func serializeQueryParam(p *ParamDescriptor, v any) []string {
	if v == nil {
		return nil
	}
	name := encodeComponent(p.Wire)
	delimiter := ","
	switch p.Style {
	case StyleSpaceDelimited:
		delimiter = "%20"
	case StylePipeDelimited:
		delimiter = "|"
	}
	if obj, ok := v.(*Object); ok && p.Style == StyleDeepObject {
		var out []string
		deepObjectPairs(name, obj, &out, 0)
		return out
	}
	switch t := v.(type) {
	case []any:
		items := encodedItems(t)
		if len(items) == 0 {
			return nil
		}
		if p.Explode || p.Style == StyleDeepObject {
			out := make([]string, len(items))
			for i, item := range items {
				out[i] = name + "=" + item
			}
			return out
		}
		return []string{name + "=" + strings.Join(items, delimiter)}
	case *Object:
		pairs := encodedPairs(t)
		if p.Explode {
			return joinedPairs(pairs)
		}
		return []string{name + "=" + strings.Join(flatPairs(pairs), delimiter)}
	}
	return []string{name + "=" + encodeComponent(scalar(v))}
}

// serializeHeaderParam renders a header parameter (simple style, not
// encoded).
func serializeHeaderParam(p *ParamDescriptor, v any) string {
	switch t := v.(type) {
	case []any:
		var parts []string
		for _, item := range t {
			if item != nil {
				parts = append(parts, scalar(item))
			}
		}
		return strings.Join(parts, ",")
	case *Object:
		var parts []string
		for _, k := range t.keys {
			item := t.vals[k]
			if item == nil {
				continue
			}
			if p.Explode {
				parts = append(parts, k+"="+scalar(item))
			} else {
				parts = append(parts, k, scalar(item))
			}
		}
		return strings.Join(parts, ",")
	}
	return scalar(v)
}

// serializeCookieParam renders name=value for a cookie parameter.
func serializeCookieParam(p *ParamDescriptor, v any) string {
	switch t := v.(type) {
	case []any:
		return p.Wire + "=" + strings.Join(encodedItems(t), ",")
	case *Object:
		return p.Wire + "=" + strings.Join(flatPairs(encodedPairs(t)), ",")
	}
	return p.Wire + "=" + encodeComponent(scalar(v))
}

// ------------------------------------------------------------------ bodies

type partKind int

const (
	partText partKind = iota
	partJSON
	partFile
)

type multipartPart struct {
	name        string
	kind        partKind
	text        string
	data        []byte
	filename    string
	contentType string
}

// payload is what goes on the wire.
type payload struct {
	empty     bool
	bytes     []byte
	multipart []multipartPart
	isParts   bool
}

var emptyPayload = payload{empty: true}

type encodedBody struct {
	payload     payload
	contentType string
	display     any
	hashed      []byte
	hasHash     bool
}

// bodyValue is the body before encoding: merged fields picked from the
// arguments (under their wire names), or args[arg]; wrapped in the rpc
// envelope when the operation has one.
func bodyValue(op *OperationDescriptor, args *Object, paramNames map[string]bool) (any, bool) {
	var value any
	has := false
	if b := op.Body; b != nil {
		if b.Arg != "" {
			value, has = args.Get(b.Arg)
		} else {
			picked := NewObject()
			for _, f := range b.Fields {
				if v, ok := args.Get(f.Arg); ok {
					picked.Set(f.Wire, v)
				}
			}
			if picked.Len() > 0 || b.Required || op.RPC != nil {
				value, has = picked, true
			}
		}
	}
	rpc := op.RPC
	if rpc == nil {
		return value, has
	}
	var params any
	if op.Body == nil {
		obj := NewObject()
		for _, k := range args.keys {
			if !paramNames[k] {
				obj.Set(k, args.vals[k])
			}
		}
		params = obj
	} else if has {
		params = value
	} else {
		params = NewObject()
	}
	envelope := NewObject()
	if rpc.Constants != nil {
		for _, k := range rpc.Constants.keys {
			envelope.Set(k, rpc.Constants.vals[k])
		}
	}
	envelope.Set(rpc.Field, rpc.Value)
	envelope.Set(rpc.ParamsField, params)
	return envelope, true
}

func formPairs(v any, parameter string) ([]pair, *serializationError) {
	o, ok := v.(*Object)
	if !ok {
		return nil, &serializationError{parameter, "an object of form fields", v}
	}
	var out []pair
	for _, k := range o.keys {
		item := o.vals[k]
		if item == nil {
			continue
		}
		if list, ok := item.([]any); ok {
			for _, x := range list {
				if x != nil {
					out = append(out, pair{k, scalar(x)})
				}
			}
		} else {
			out = append(out, pair{k, scalar(item)})
		}
	}
	return out, nil
}

func binaryDisplay(n int, media string) string {
	if media == "" {
		return fmt.Sprintf("<%d bytes>", n)
	}
	return fmt.Sprintf("<%d bytes of %s>", n, media)
}

// encodeBody encodes the value per the body descriptor.
func encodeBody(d *BodyDescriptor, v any, has bool, redact func(any) any) (encodedBody, *serializationError) {
	if !has {
		return encodedBody{payload: emptyPayload, display: nil}, nil
	}
	encoding := EncodeJSON
	media := "application/json"
	if d != nil {
		encoding, media = d.Encoding, d.MediaType
	}
	const parameter = "body"
	switch encoding {
	case EncodeForm:
		pairs, err := formPairs(v, parameter)
		if err != nil {
			return encodedBody{}, err
		}
		parts := make([]string, len(pairs))
		raw := NewObject()
		for i, p := range pairs {
			parts[i] = encodeFormComponent(p.k) + "=" + encodeFormComponent(p.v)
			raw.Set(p.k, p.v)
		}
		text := strings.Join(parts, "&")
		return encodedBody{payload: payload{bytes: []byte(text)}, contentType: media, display: redact(raw), hashed: []byte(text), hasHash: true}, nil
	case EncodeMultipart:
		o, ok := v.(*Object)
		if !ok {
			return encodedBody{}, &serializationError{parameter, "an object of multipart fields", v}
		}
		var parts []multipartPart
		display := NewObject()
		for _, k := range o.keys {
			item := o.vals[k]
			if item == nil {
				continue
			}
			list := []any{item}
			arr, isArray := item.([]any)
			if isArray {
				list = nil
				for _, x := range arr {
					if x != nil {
						list = append(list, x)
					}
				}
			}
			shown := []any{}
			for _, x := range list {
				if bin, ok := BinaryOf(x); ok {
					shown = append(shown, binaryDisplay(len(bin.Data), bin.ContentType))
					name := bin.Filename
					if name == "" {
						name = k
					}
					ct := bin.ContentType
					if ct == "" {
						ct = "application/octet-stream"
					}
					parts = append(parts, multipartPart{name: k, kind: partFile, data: bin.Data, filename: name, contentType: ct})
				} else if _, isObj := x.(*Object); isObj {
					parts = append(parts, multipartPart{name: k, kind: partJSON, text: JSONText(x)})
					shown = append(shown, x)
				} else {
					parts = append(parts, multipartPart{name: k, kind: partText, text: scalar(x)})
					shown = append(shown, scalar(x))
				}
			}
			if isArray {
				display.Set(k, shown)
			} else if len(shown) > 0 {
				display.Set(k, shown[0])
			} else {
				display.Set(k, nil)
			}
		}
		return encodedBody{payload: payload{multipart: parts, isParts: true}, display: redact(display), hashed: []byte(CanonicalJSON(v)), hasHash: true}, nil
	case EncodeBytes:
		var data []byte
		if text, ok := v.(string); ok {
			data = []byte(text)
		} else if bin, ok := BinaryOf(v); ok {
			data = bin.Data
		} else {
			return encodedBody{}, &serializationError{parameter, "binary data (a Binary value)", v}
		}
		return encodedBody{payload: payload{bytes: data}, contentType: media, display: binaryDisplay(len(data), media), hashed: data, hasHash: true}, nil
	case EncodeText:
		switch v.(type) {
		case string, int64, float64, bool:
		default:
			return encodedBody{}, &serializationError{parameter, "a string", v}
		}
		text := scalar(v)
		return encodedBody{payload: payload{bytes: []byte(text)}, contentType: media, display: text, hashed: []byte(text), hasHash: true}, nil
	}
	text := JSONText(v)
	raw, err := ParseJSONString(text)
	if err != nil {
		return encodedBody{}, &serializationError{parameter, "a JSON-serializable value", v}
	}
	return encodedBody{payload: payload{bytes: []byte(text)}, contentType: media, display: redact(raw), hashed: []byte(CanonicalJSON(raw)), hasHash: true}, nil
}

// validHeaderName: an RFC 9110 token.
func validHeaderName(name string) bool {
	if name == "" {
		return false
	}
	for i := 0; i < len(name); i++ {
		if !isUnreserved(name[i], "!#$%&'*+-.^_`|~") {
			return false
		}
	}
	return true
}

// validHeaderValue: visible ASCII, tab, space and Latin-1 characters.
func validHeaderValue(value string) bool {
	for _, r := range value {
		if !(r == '\t' || (r >= 0x20 && r <= 0x7e) || (r >= 0x80 && r <= 0xff)) {
			return false
		}
	}
	return true
}

// headerValueBytes is the Latin-1 bytes of a valid header value.
func headerValueBytes(value string) string {
	var b strings.Builder
	for _, r := range value {
		b.WriteByte(byte(r))
	}
	return b.String()
}

type headerEntry struct {
	name   string
	value  string
	secret bool
}

// headerBag is a case-insensitive header collection that remembers which
// values are secrets.
type headerBag struct {
	entries []headerEntry
}

func (h *headerBag) set(name, value string, secret bool) {
	for i := range h.entries {
		if strings.EqualFold(h.entries[i].name, name) {
			h.entries[i] = headerEntry{name, value, secret}
			return
		}
	}
	h.entries = append(h.entries, headerEntry{name, value, secret})
}

func (h *headerBag) get(name string) (string, bool) {
	for _, e := range h.entries {
		if strings.EqualFold(e.name, name) {
			return e.value, true
		}
	}
	return "", false
}

func (h *headerBag) toMap(redact bool) map[string]string {
	out := make(map[string]string, len(h.entries))
	for _, e := range h.entries {
		if redact && e.secret {
			out[e.name] = Redacted
		} else {
			out[e.name] = e.value
		}
	}
	return out
}

func (h *headerBag) secrets() []string {
	var out []string
	for _, e := range h.entries {
		if e.secret {
			out = append(out, e.value)
		}
	}
	return out
}
