// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"strconv"
	"strings"
	"unicode/utf8"
)

// A server-sent events parser (WHATWG HTML 9.2): lines end with CRLF, LF or
// CR; a line is a comment (":"), a field (event, data, id, retry; the
// value loses one leading space) or the blank line that dispatches the
// event. data lines are joined with LF; an event without data lines is not
// dispatched; an event still open when the stream ends is discarded. An
// event may hold at most max bytes (its field lines, each with a one-byte
// terminator, plus the line being read); past it the parser stops.

// sseEvent is one dispatched event.
type sseEvent struct {
	event string
	data  string
	// id and retry persist across events.
	id       string
	hasID    bool
	retry    int64
	hasRetry bool
}

type sseParser struct {
	buffer     strings.Builder
	pendingCR  bool
	started    bool
	event      string
	data       strings.Builder
	hasData    bool
	id         string
	hasID      bool
	retry      int64
	hasRetry   bool
	eventBytes int
	exceeded   bool
	max        int
}

func newSSEParser(max int) *sseParser {
	return &sseParser{max: max}
}

// push feeds decoded text and returns the events it completes. A CR at the
// end is held back until the next text shows whether an LF follows.
func (p *sseParser) push(chunk string) []sseEvent {
	if p.exceeded {
		return nil
	}
	if !p.started && chunk != "" {
		p.started = true
		chunk = strings.TrimPrefix(chunk, "\uFEFF")
	}
	var out []sseEvent
	if p.pendingCR {
		if chunk == "" {
			return out
		}
		p.pendingCR = false
		p.complete(&out)
		chunk = strings.TrimPrefix(chunk, "\n")
	}
	start := 0
	for i := 0; i < len(chunk) && !p.exceeded; i++ {
		c := chunk[i]
		if c != '\n' && c != '\r' {
			continue
		}
		p.buffer.WriteString(chunk[start:i])
		if c == '\r' && i+1 == len(chunk) {
			p.pendingCR = true
			p.checkLimit(1)
			return out
		}
		p.complete(&out)
		if c == '\r' && chunk[i+1] == '\n' {
			i++
		}
		start = i + 1
	}
	if !p.exceeded && p.checkLimit(len(chunk)-start) {
		p.buffer.WriteString(chunk[start:])
	}
	return out
}

func (p *sseParser) checkLimit(extra int) bool {
	if p.eventBytes > p.max || p.buffer.Len() > p.max-p.eventBytes || extra > p.max-p.eventBytes-p.buffer.Len() {
		p.exceeded = true
	}
	if p.exceeded {
		p.buffer.Reset()
		p.data.Reset()
		p.event = ""
		p.id = ""
		p.hasID = false
		p.hasData = false
	}
	return !p.exceeded
}

func (p *sseParser) complete(out *[]sseEvent) {
	line := p.buffer.String()
	p.buffer.Reset()
	p.line(line, out)
	p.checkLimit(0)
}

// end: a held CR ends its line; any open event is discarded.
func (p *sseParser) end() []sseEvent {
	var out []sseEvent
	if !p.exceeded && p.pendingCR {
		p.complete(&out)
	}
	p.pendingCR = false
	p.buffer.Reset()
	p.event = ""
	p.data.Reset()
	p.hasData = false
	p.eventBytes = 0
	return out
}

func (p *sseParser) line(line string, out *[]sseEvent) {
	if line == "" {
		if p.hasData {
			data := p.data.String()
			data = data[:len(data)-1]
			event := p.event
			if event == "" {
				event = "message"
			}
			*out = append(*out, sseEvent{event: event, data: data, id: p.id, hasID: p.hasID, retry: p.retry, hasRetry: p.hasRetry})
		}
		p.event = ""
		p.data.Reset()
		p.hasData = false
		p.eventBytes = 0
		return
	}
	if strings.HasPrefix(line, ":") {
		return
	}
	p.eventBytes += len(line) + 1
	if p.eventBytes > p.max {
		p.exceeded = true
		return
	}
	name, value, _ := strings.Cut(line, ":")
	value = strings.TrimPrefix(value, " ")
	switch name {
	case "event":
		p.event = value
	case "data":
		p.data.WriteString(value)
		p.data.WriteByte('\n')
		p.hasData = true
	case "id":
		if !strings.Contains(value, "\x00") {
			p.id, p.hasID = value, true
		}
	case "retry":
		if allDigits(value) {
			digits := strings.TrimLeft(value, "0")
			if digits == "" {
				digits = "0"
			}
			if ms, err := strconv.ParseInt(digits, 10, 64); err == nil && ms <= 9007199254740991 {
				p.retry, p.hasRetry = ms, true
			}
		}
	}
}

// utf8Decoder joins sequences split across chunks; invalid bytes become
// U+FFFD.
type utf8Decoder struct {
	pending []byte
}

func (d *utf8Decoder) decode(data []byte) string {
	buf := append(d.pending, data...)
	d.pending = nil
	var b strings.Builder
	for len(buf) > 0 {
		r, size := utf8.DecodeRune(buf)
		if r == utf8.RuneError && size <= 1 {
			if !utf8.FullRune(buf) {
				d.pending = append([]byte(nil), buf...)
				return b.String()
			}
			b.WriteRune(utf8.RuneError)
			buf = buf[1:]
			continue
		}
		b.Write(buf[:size])
		buf = buf[size:]
	}
	return b.String()
}

func (d *utf8Decoder) finish() string {
	if len(d.pending) == 0 {
		return ""
	}
	d.pending = nil
	return "\uFFFD"
}
