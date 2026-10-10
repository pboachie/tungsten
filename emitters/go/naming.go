// SPDX-License-Identifier: Apache-2.0

package main

import (
	"strconv"
	"strings"
)

// splitWords is tungsten_ir::naming::split_words: ASCII alphanumeric runs
// split at case changes and digits, lower-cased.
func splitWords(wire string) []string {
	var out []string
	chunks := strings.FieldsFunc(wire, func(r rune) bool {
		return !((r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') || (r >= '0' && r <= '9'))
	})
	for _, chunk := range chunks {
		var cur strings.Builder
		hasLetter := false
		for i := 0; i < len(chunk); i++ {
			c := chunk[i]
			if i > 0 && startsWord(chunk, i, hasLetter) {
				out = append(out, cur.String())
				cur.Reset()
				hasLetter = false
			}
			if isLetter(c) {
				hasLetter = true
			}
			cur.WriteByte(lower(c))
		}
		if cur.Len() > 0 {
			out = append(out, cur.String())
		}
	}
	return out
}

func isLetter(c byte) bool { return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') }
func isUpper(c byte) bool  { return c >= 'A' && c <= 'Z' }
func isLower(c byte) bool  { return c >= 'a' && c <= 'z' }
func isDigit(c byte) bool  { return c >= '0' && c <= '9' }

func lower(c byte) byte {
	if isUpper(c) {
		return c + 32
	}
	return c
}

func startsWord(chunk string, i int, wordHasLetter bool) bool {
	prev, cur := chunk[i-1], chunk[i]
	if isDigit(prev) {
		return isLetter(cur) && wordHasLetter
	}
	if isLower(prev) {
		return isUpper(cur)
	}
	if !isUpper(cur) {
		return false
	}
	if i+1 >= len(chunk) {
		return false
	}
	next := chunk[i+1]
	if next == 's' {
		return i+2 < len(chunk) && isLower(chunk[i+2])
	}
	return isLower(next)
}

func cleanWords(words []string) []string {
	var out []string
	for _, w := range words {
		if w != "" {
			out = append(out, strings.ToLower(w))
		}
	}
	return out
}

func capitalize(w string) string {
	if w == "" {
		return w
	}
	return strings.ToUpper(w[:1]) + w[1:]
}

func joinCapitalized(words []string, first bool) string {
	var b strings.Builder
	for i, w := range words {
		s := b.String()
		if len(s) > 0 && isDigit(s[len(s)-1]) && isDigit(w[0]) {
			b.WriteByte('_')
		}
		if i == 0 && !first {
			b.WriteString(w)
		} else {
			b.WriteString(capitalize(w))
		}
	}
	return b.String()
}

// camel and pascal are tungsten_ir::naming::to_case.
func camel(words []string) string {
	w := cleanWords(words)
	if len(w) == 0 {
		return "_"
	}
	return joinCapitalized(w, false)
}

func withDigitPrefix(name string) string {
	if name != "" && isDigit(name[0]) {
		return "_" + name
	}
	return name
}

var tsReserved = map[string]bool{}

func init() {
	for _, w := range strings.Fields(`any arguments await bigint boolean break case catch class const continue
		debugger default delete do else enum eval export extends false finally for function if implements
		import in instanceof interface let never new null number object package private protected public
		return static string super switch symbol this throw true try type typeof undefined unknown var void
		while with yield`) {
		tsReserved[w] = true
	}
}

// tsParam renders words as a TypeScript parameter name (the arguments
// object key every SDK and tool manifest shares).
func tsParam(words []string) string {
	name := withDigitPrefix(camel(words))
	if tsReserved[name] {
		name += "_"
	}
	return name
}

// disambiguate makes the rendered names of one scope unique by appending a
// numeric word to later collisions (tungsten_ir::naming::disambiguate).
func disambiguate(entries [][]string, render func([]string) string) []string {
	rendered := make([]string, len(entries))
	taken := map[string]bool{}
	for i, e := range entries {
		rendered[i] = render(e)
		taken[rendered[i]] = true
	}
	claimed := map[string]bool{}
	next := map[string]int{}
	out := make([]string, len(entries))
	for i, e := range entries {
		name := rendered[i]
		if !claimed[name] {
			claimed[name] = true
			out[i] = name
			continue
		}
		key := strings.Join(e, "\x00")
		n := next[key]
		if n == 0 {
			n = 2
		}
		for {
			candidate := render(append(append([]string(nil), e...), strconv.Itoa(n)))
			if !taken[candidate] {
				taken[candidate] = true
				next[key] = n + 1
				out[i] = candidate
				break
			}
			n++
		}
	}
	return out
}

// Go identifiers: Pascal case with the common initialisms upper-cased
// (ID, URL, HTTP).
var initialisms = map[string]string{
	"id": "ID", "ids": "IDs", "url": "URL", "urls": "URLs", "uri": "URI", "uuid": "UUID", "http": "HTTP",
	"https": "HTTPS", "api": "API", "json": "JSON", "xml": "XML", "html": "HTML", "ip": "IP", "tls": "TLS",
	"ttl": "TTL", "dns": "DNS", "rpc": "RPC", "sql": "SQL", "sms": "SMS", "csrf": "CSRF", "jwt": "JWT",
}

// goName renders words as an exported Go identifier.
func goName(words []string) string {
	w := cleanWords(words)
	if len(w) == 0 {
		return "X"
	}
	var b strings.Builder
	for _, word := range w {
		s := b.String()
		if len(s) > 0 && isDigit(s[len(s)-1]) && isDigit(word[0]) {
			b.WriteByte('_')
		}
		if up, ok := initialisms[word]; ok {
			b.WriteString(up)
		} else {
			b.WriteString(capitalize(word))
		}
	}
	name := b.String()
	if isDigit(name[0]) {
		name = "N" + name
	}
	return name
}

// goNames renders unique exported names in one scope; reserved names are
// taken already.
func goNames(entries [][]string, reserved ...string) []string {
	all := make([][]string, 0, len(reserved)+len(entries))
	for _, r := range reserved {
		all = append(all, splitWords(r))
	}
	all = append(all, entries...)
	names := disambiguate(all, goName)
	return names[len(reserved):]
}

func goString(s string) string {
	return strconv.Quote(s)
}

// packageName is a Go package name from text: lower-case letters and
// digits, starting with a letter.
func packageName(text string) string {
	var b strings.Builder
	for _, r := range strings.ToLower(text) {
		if (r >= 'a' && r <= 'z') || (r >= '0' && r <= '9' && b.Len() > 0) {
			b.WriteRune(r)
		}
	}
	name := b.String()
	if name == "" || goKeywords[name] {
		return "sdk"
	}
	return name
}

var goKeywords = map[string]bool{
	"break": true, "case": true, "chan": true, "const": true, "continue": true, "default": true,
	"defer": true, "else": true, "fallthrough": true, "for": true, "func": true, "go": true, "goto": true,
	"if": true, "import": true, "interface": true, "map": true, "package": true, "range": true,
	"return": true, "select": true, "struct": true, "switch": true, "type": true, "var": true,
}
