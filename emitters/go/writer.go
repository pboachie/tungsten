// SPDX-License-Identifier: Apache-2.0

package main

import (
	"fmt"
	"go/format"
	"strings"
)

// writer collects Go source lines; gofmt (go/format) lays them out, so
// lines are written without indentation.
type writer struct {
	b strings.Builder
}

func (w *writer) line(format string, args ...any) {
	if len(args) == 0 {
		w.b.WriteString(format)
	} else {
		fmt.Fprintf(&w.b, format, args...)
	}
	w.b.WriteByte('\n')
}

func (w *writer) raw(text string) {
	w.b.WriteString(text)
}

// comment writes text as // comments wrapped at 96 columns; blank lines
// separate paragraphs.
func (w *writer) comment(text string) {
	for i, para := range strings.Split(strings.TrimSpace(text), "\n\n") {
		if i > 0 {
			w.line("//")
		}
		for _, l := range wrap(strings.Join(strings.Fields(para), " "), 96) {
			w.line("// %s", l)
		}
	}
}

func wrap(text string, width int) []string {
	words := strings.Fields(text)
	var out []string
	var cur strings.Builder
	for _, word := range words {
		if cur.Len() > 0 && cur.Len()+1+len(word) > width {
			out = append(out, cur.String())
			cur.Reset()
		}
		if cur.Len() > 0 {
			cur.WriteByte(' ')
		}
		cur.WriteString(word)
	}
	if cur.Len() > 0 {
		out = append(out, cur.String())
	}
	return out
}

// source formats the collected file.
func (w *writer) source(path string) (string, error) {
	out, err := format.Source([]byte(w.b.String()))
	if err != nil {
		return "", fmt.Errorf("%s does not format: %w", path, err)
	}
	return string(out), nil
}
