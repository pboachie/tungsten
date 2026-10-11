// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"bytes"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"math"
	"strconv"
	"strings"
	"time"
)

// Redacted is written wherever a secret or sensitive value would appear.
const Redacted = "<redacted>"

// maxReceivedChars is the longest string kept in received_value.
const maxReceivedChars = 200

// maxArgDepth is the deepest JSON nesting accepted in arguments.
const maxArgDepth = 256

// maxSafeInteger is the largest integer a JavaScript number holds exactly.
const maxSafeInteger = 9007199254740991.0

func base64Standard(data []byte) string {
	return base64.StdEncoding.EncodeToString(data)
}

func base64URL(data []byte) string {
	return base64.RawURLEncoding.EncodeToString(data)
}

func sha256Hex(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

func hmacSHA256(key, message []byte) []byte {
	mac := hmac.New(sha256.New, key)
	mac.Write(message)
	return mac.Sum(nil)
}

func timingSafeEqual(a, b string) bool {
	if len(a) != len(b) {
		return false
	}
	return subtle.ConstantTimeCompare([]byte(a), []byte(b)) == 1
}

func randomBytes(n int) ([]byte, bool) {
	out := make([]byte, n)
	if _, err := rand.Read(out); err != nil {
		return nil, false
	}
	return out, true
}

// uuidV4 is a random UUID version 4 in canonical lowercase form.
func uuidV4() (string, bool) {
	b, ok := randomBytes(16)
	if !ok {
		return "", false
	}
	b[6] = (b[6] & 0x0f) | 0x40
	b[8] = (b[8] & 0x3f) | 0x80
	h := hex.EncodeToString(b)
	return h[0:8] + "-" + h[8:12] + "-" + h[12:16] + "-" + h[16:20] + "-" + h[20:32], true
}

// NullMembers returns the top-level members of a JSON object that are
// null; generated models use it to tell an explicit null (Null) from an
// absent member (a nil *Patch).
func NullMembers(data []byte) map[string]bool {
	var raw map[string]json.RawMessage
	if json.Unmarshal(data, &raw) != nil {
		return nil
	}
	out := map[string]bool{}
	for k, v := range raw {
		if string(bytes.TrimSpace(v)) == "null" {
			out[k] = true
		}
	}
	return out
}

// ------------------------------------------------------------------ values

func nestsDeeperThan(v any, limit int) bool {
	switch t := v.(type) {
	case []any:
		if limit == 0 {
			return true
		}
		for _, item := range t {
			if nestsDeeperThan(item, limit-1) {
				return true
			}
		}
	case *Object:
		if limit == 0 {
			return true
		}
		for _, k := range t.keys {
			if nestsDeeperThan(t.vals[k], limit-1) {
				return true
			}
		}
	}
	return false
}

// durationMs is a duration of ms milliseconds; negative and NaN are zero,
// huge values capped.
func durationMs(ms float64) time.Duration {
	if math.IsNaN(ms) || ms <= 0 {
		return 0
	}
	seconds := ms / 1000
	if seconds > float64(math.MaxUint32) {
		seconds = float64(math.MaxUint32)
	}
	return time.Duration(seconds * float64(time.Second))
}

// splitPath splits a field path (a.b, items[0].id, items.0.id).
func splitPath(path string) []string {
	if path == "" || path == "." {
		return nil
	}
	var dotted strings.Builder
	for i := 0; i < len(path); {
		if path[i] == '[' {
			j := i + 1
			for j < len(path) && path[j] >= '0' && path[j] <= '9' {
				j++
			}
			if j > i+1 && j < len(path) && path[j] == ']' {
				dotted.WriteByte('.')
				dotted.WriteString(path[i+1 : j])
				i = j + 1
				continue
			}
		}
		dotted.WriteByte(path[i])
		i++
	}
	var out []string
	for _, s := range strings.Split(dotted.String(), ".") {
		if s != "" {
			out = append(out, s)
		}
	}
	return out
}

func isIndex(s string) bool {
	if s == "" {
		return false
	}
	for i := 0; i < len(s); i++ {
		if s[i] < '0' || s[i] > '9' {
			return false
		}
	}
	return true
}

// getPath reads a field path; ok is false when absent.
func getPath(v any, present bool, segments []string) (any, bool) {
	if !present {
		return nil, false
	}
	cur := v
	for _, seg := range segments {
		switch t := cur.(type) {
		case []any:
			if !isIndex(seg) {
				return nil, false
			}
			n, err := strconv.Atoi(seg)
			if err != nil || n >= len(t) {
				return nil, false
			}
			cur = t[n]
		case *Object:
			next, ok := t.Get(seg)
			if !ok {
				return nil, false
			}
			cur = next
		default:
			return nil, false
		}
	}
	return cur, true
}

func getPathStr(v any, present bool, path string) (any, bool) {
	return getPath(v, present, splitPath(path))
}

func asFloat(v any) (float64, bool) {
	switch t := v.(type) {
	case int64:
		return float64(t), true
	case float64:
		return t, true
	}
	return 0, false
}

func numbersEqual(a, b any) bool {
	ai, aInt := a.(int64)
	bi, bInt := b.(int64)
	if aInt && bInt {
		return ai == bi
	}
	af, _ := asFloat(a)
	bf, _ := asFloat(b)
	return af == bf
}

// deepEqual is structural equality (1 equals 1.0; member order does not
// matter).
func deepEqual(a, b any) bool {
	switch x := a.(type) {
	case nil:
		return b == nil
	case bool:
		y, ok := b.(bool)
		return ok && x == y
	case string:
		y, ok := b.(string)
		return ok && x == y
	case int64, float64:
		switch b.(type) {
		case int64, float64:
			return numbersEqual(a, b)
		}
		return false
	case []any:
		y, ok := b.([]any)
		if !ok || len(x) != len(y) {
			return false
		}
		for i := range x {
			if !deepEqual(x[i], y[i]) {
				return false
			}
		}
		return true
	case *Object:
		y, ok := b.(*Object)
		if !ok || x.Len() != y.Len() {
			return false
		}
		for _, k := range x.keys {
			other, ok := y.Get(k)
			if !ok || !deepEqual(x.vals[k], other) {
				return false
			}
		}
		return true
	}
	return false
}

// deepEqualOpt is deepEqual where either side may be absent.
func deepEqualOpt(a any, aOK bool, b any, bOK bool) bool {
	if !aOK || !bOK {
		return aOK == bOK
	}
	return deepEqual(a, b)
}

// containsSubset: every field of pattern appears in value with an equal
// (recursively contained) value; arrays match when each pattern element is
// contained by some element.
func containsSubset(value any, present bool, pattern any) bool {
	switch p := pattern.(type) {
	case *Object:
		have, ok := value.(*Object)
		if !present || !ok {
			return false
		}
		for _, k := range p.keys {
			v, ok := have.Get(k)
			if !ok || !containsSubset(v, true, p.vals[k]) {
				return false
			}
		}
		return true
	case []any:
		have, ok := value.([]any)
		if !present || !ok {
			return false
		}
		for _, want := range p {
			found := false
			for _, h := range have {
				if containsSubset(h, true, want) {
					found = true
					break
				}
			}
			if !found {
				return false
			}
		}
		return true
	default:
		return present && deepEqual(value, pattern)
	}
}

// ---------------------------------------------------------------- redaction

var sensitiveWords = []string{
	"secret", "password", "passwd", "passphrase", "token", "apikey", "api-key", "api_key",
	"privatekey", "private-key", "private_key", "credential", "authorization", "cookie", "session",
}

// looksSensitive reports whether a name very likely holds a secret.
func looksSensitive(name string) bool {
	lower := strings.ToLower(name)
	for _, w := range sensitiveWords {
		if strings.Contains(lower, w) {
			return true
		}
	}
	return false
}

func redactSensitiveKeys(v any, depth int) any {
	if depth > 32 {
		return nil
	}
	switch t := v.(type) {
	case []any:
		out := make([]any, len(t))
		for i, item := range t {
			out[i] = redactSensitiveKeys(item, depth+1)
		}
		return out
	case *Object:
		out := NewObject()
		for _, k := range t.keys {
			if looksSensitive(k) {
				out.Set(k, Redacted)
			} else {
				out.Set(k, redactSensitiveKeys(t.vals[k], depth+1))
			}
		}
		return out
	}
	return v
}

func redactAt(v any, segments []string) any {
	if len(segments) == 0 {
		return Redacted
	}
	head, rest := segments[0], segments[1:]
	switch t := v.(type) {
	case []any:
		out := make([]any, len(t))
		for i, item := range t {
			out[i] = redactAt(item, segments)
		}
		return out
	case *Object:
		if !t.Has(head) {
			return t
		}
		out := t.Clone()
		out.Set(head, redactAt(t.vals[head], rest))
		return out
	}
	return v
}

// redactPaths copies v with every listed field path redacted; arrays on the
// way fan out.
func redactPaths(v any, paths []string) any {
	out := DeepCopy(v)
	for _, p := range paths {
		segs := splitPath(p)
		if len(segs) > 0 {
			out = redactAt(out, segs)
		}
	}
	return out
}

func cut(text string) string {
	if utf16Len(text) > maxReceivedChars {
		return takeUnits(text, maxReceivedChars-1) + "…"
	}
	return text
}

// envelopeValue makes a value safe for received_value.
func envelopeValue(v any, sensitive bool) any {
	if sensitive {
		return Redacted
	}
	switch t := v.(type) {
	case string:
		return cut(t)
	case []any, *Object:
		if bin, ok := BinaryOf(t); ok {
			return "<" + strconv.Itoa(len(bin.Data)) + " bytes>"
		}
		text := JSONText(redactSensitiveKeys(t, 0))
		if utf16Len(text) > maxReceivedChars {
			return cut(text)
		}
		parsed, err := ParseJSONString(text)
		if err != nil {
			return nil
		}
		return parsed
	}
	return v
}

// secretSet holds secret values to scrub from texts, in insertion order.
type secretSet struct {
	items []string
}

func (s *secretSet) insert(secret string) {
	if secret == "" {
		return
	}
	for _, it := range s.items {
		if it == secret {
			return
		}
	}
	s.items = append(s.items, secret)
}

func (s *secretSet) empty() bool { return len(s.items) == 0 }

func (s *secretSet) clone() secretSet {
	return secretSet{items: append([]string(nil), s.items...)}
}

// scrubText replaces every occurrence of the secrets (four characters or
// more).
func scrubText(text string, secrets *secretSet) string {
	for _, secret := range secrets.items {
		if len([]rune(secret)) >= 4 && strings.Contains(text, secret) {
			text = strings.ReplaceAll(text, secret, Redacted)
		}
	}
	return text
}

func scrubValue(v any, secrets *secretSet) any {
	switch t := v.(type) {
	case string:
		return scrubText(t, secrets)
	case []any, *Object:
		scrubbed := scrubText(JSONText(t), secrets)
		parsed, err := ParseJSONString(scrubbed)
		if err != nil {
			return Redacted
		}
		return parsed
	}
	return v
}

// mergeAccept is the Accept of a stream call: the caller's media ranges in
// order without repeats, text/event-stream first when they do not name it.
func mergeAccept(value string) string {
	var ranges []string
	for _, part := range strings.Split(value, ",") {
		r := strings.TrimSpace(part)
		if r == "" {
			continue
		}
		dup := false
		for _, x := range ranges {
			if x == r {
				dup = true
				break
			}
		}
		if !dup {
			ranges = append(ranges, r)
		}
	}
	names := false
	for _, r := range ranges {
		essence, _, _ := strings.Cut(r, ";")
		if strings.EqualFold(strings.TrimSpace(essence), "text/event-stream") {
			names = true
		}
	}
	if !names {
		ranges = append([]string{"text/event-stream"}, ranges...)
	}
	return strings.Join(ranges, ", ")
}
