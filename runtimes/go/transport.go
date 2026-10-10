// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"io"
	"mime/multipart"
	"net"
	"net/http"
	"net/http/httptrace"
	"net/textproto"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"
)

// newHTTPClient is the default client: no redirects (the runtime follows
// same-origin redirects of reads itself, and never on mutations), no
// compression, no proxy for loopback addresses beyond the environment's
// rules, and no timeout of its own (each attempt has a deadline).
func newHTTPClient() *http.Client {
	transport := http.DefaultTransport.(*http.Transport).Clone()
	transport.DisableCompression = true
	return &http.Client{
		Transport: transport,
		CheckRedirect: func(*http.Request, []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
}

type attemptRequest struct {
	url     string
	method  string
	headers []headerEntry
	body    *payload
	timeout time.Duration
	// stream hands a 2xx text/event-stream answer back unread.
	stream bool
}

type outcomeKind int

const (
	outcomeResponse outcomeKind = iota
	outcomeStreaming
	outcomeNotSent
	outcomeLost
	outcomeTimeout
	// outcomeAborted: the caller's context ended the attempt; sent says
	// whether the request had been written.
	outcomeAborted
)

// attemptOutcome is what one attempt gave.
type attemptOutcome struct {
	kind    outcomeKind
	status  int
	headers map[string]string
	// body is nil with bodyFailure set when reading it failed.
	body        []byte
	bodyOK      bool
	bodyFailure string
	stream      *eventBody
	detail      string
	sent        bool
}

// eventBody is the unread body of an event stream; each read waits at most
// the attempt timeout.
type eventBody struct {
	response *http.Response
	cancel   context.CancelFunc
	idle     time.Duration
	buf      []byte
}

type bodyRead int

const (
	readChunk bodyRead = iota
	readEnd
	readTimeout
	readLost
)

func (e *eventBody) read() ([]byte, bodyRead) {
	if e.buf == nil {
		e.buf = make([]byte, 32*1024)
	}
	var fired bool
	var mu sync.Mutex
	timer := time.AfterFunc(e.idle, func() {
		mu.Lock()
		fired = true
		mu.Unlock()
		e.cancel()
	})
	n, err := e.response.Body.Read(e.buf)
	timer.Stop()
	mu.Lock()
	timedOut := fired
	mu.Unlock()
	if n > 0 {
		out := make([]byte, n)
		copy(out, e.buf[:n])
		return out, readChunk
	}
	switch {
	case err == io.EOF:
		return nil, readEnd
	case timedOut:
		return nil, readTimeout
	case err != nil:
		return nil, readLost
	}
	return nil, readChunk
}

func (e *eventBody) close() {
	e.cancel()
	_ = e.response.Body.Close()
}

func isEventStream(headers map[string]string) bool {
	v, ok := headers["content-type"]
	if !ok {
		return false
	}
	essence, _, _ := strings.Cut(v, ";")
	return strings.EqualFold(strings.TrimSpace(essence), "text/event-stream")
}

// errorCodes are short secret-free codes describing a transport error.
func errorCodes(err error) []string {
	var codes []string
	push := func(code string) {
		for _, c := range codes {
			if c == code {
				return
			}
		}
		codes = append(codes, code)
	}
	var errno syscall.Errno
	if errors.As(err, &errno) {
		switch errno {
		case syscall.ECONNREFUSED:
			push("ECONNREFUSED")
		case syscall.ECONNRESET:
			push("ECONNRESET")
		case syscall.ECONNABORTED:
			push("ECONNABORTED")
		case syscall.ENOTCONN:
			push("ENOTCONN")
		case syscall.EPIPE:
			push("EPIPE")
		case syscall.ETIMEDOUT:
			push("ETIMEDOUT")
		case syscall.EADDRNOTAVAIL:
			push("EADDRNOTAVAIL")
		case syscall.EHOSTUNREACH:
			push("EHOSTUNREACH")
		case syscall.ENETUNREACH:
			push("ENETUNREACH")
		}
	}
	var dns *net.DNSError
	if errors.As(err, &dns) {
		push("ENOTFOUND")
	}
	var unknownAuthority x509.UnknownAuthorityError
	var hostname x509.HostnameError
	var invalid x509.CertificateInvalidError
	var verification *tls.CertificateVerificationError
	switch {
	case errors.As(err, &unknownAuthority), errors.As(err, &hostname), errors.As(err, &invalid), errors.As(err, &verification):
		push("CERT_VERIFY_FAILED")
	default:
		var record tls.RecordHeaderError
		var alert tls.AlertError
		if errors.As(err, &record) || errors.As(err, &alert) || strings.Contains(strings.ToLower(err.Error()), "tls") {
			push("TLS_ERROR")
		}
	}
	if errors.Is(err, io.EOF) || errors.Is(err, io.ErrUnexpectedEOF) || strings.Contains(err.Error(), "server closed") {
		push("CONNECTION_CLOSED")
	}
	return codes
}

// notSentError reports whether a failure happened before the request could
// reach the server: DNS, connect, TLS handshake.
func notSentError(err error) bool {
	var op *net.OpError
	if errors.As(err, &op) && op.Op == "dial" {
		return true
	}
	var dns *net.DNSError
	if errors.As(err, &dns) {
		return true
	}
	var unknownAuthority x509.UnknownAuthorityError
	var hostname x509.HostnameError
	var invalid x509.CertificateInvalidError
	var verification *tls.CertificateVerificationError
	var record tls.RecordHeaderError
	return errors.As(err, &unknownAuthority) || errors.As(err, &hostname) || errors.As(err, &invalid) ||
		errors.As(err, &verification) || errors.As(err, &record)
}

func failureDetail(err error, sent bool) string {
	codes := errorCodes(err)
	if len(codes) == 0 {
		if !sent {
			return "connect error"
		}
		return "network error"
	}
	return strings.Join(codes, ", ")
}

func multipartBody(parts []multipartPart) ([]byte, string, error) {
	var buf bytes.Buffer
	w := multipart.NewWriter(&buf)
	for _, p := range parts {
		h := textproto.MIMEHeader{}
		switch p.kind {
		case partFile:
			h.Set("Content-Disposition", fmt.Sprintf(`form-data; name="%s"; filename="%s"`, escapeQuotes(p.name), escapeQuotes(p.filename)))
			h.Set("Content-Type", p.contentType)
		case partJSON:
			h.Set("Content-Disposition", fmt.Sprintf(`form-data; name="%s"`, escapeQuotes(p.name)))
			h.Set("Content-Type", "application/json")
		default:
			h.Set("Content-Disposition", fmt.Sprintf(`form-data; name="%s"`, escapeQuotes(p.name)))
		}
		part, err := w.CreatePart(h)
		if err != nil {
			return nil, "", err
		}
		if p.kind == partFile {
			_, err = part.Write(p.data)
		} else {
			_, err = io.WriteString(part, p.text)
		}
		if err != nil {
			return nil, "", err
		}
	}
	if err := w.Close(); err != nil {
		return nil, "", err
	}
	return buf.Bytes(), w.FormDataContentType(), nil
}

var quoteEscaper = strings.NewReplacer("\\", "\\\\", `"`, "\\\"", "\r", "%0D", "\n", "%0A")

func escapeQuotes(s string) string { return quoteEscaper.Replace(s) }

// attempt sends once. It never fails: every outcome is an attemptOutcome.
func attempt(ctx context.Context, client *http.Client, req attemptRequest) attemptOutcome {
	var body io.Reader
	contentType := ""
	switch {
	case req.body == nil || req.body.empty:
	case req.body.isParts:
		data, ct, err := multipartBody(req.body.multipart)
		if err != nil {
			return attemptOutcome{kind: outcomeNotSent, detail: "INVALID_BODY"}
		}
		body, contentType = bytes.NewReader(data), ct
	default:
		body = bytes.NewReader(req.body.bytes)
	}
	attemptCtx, cancel := context.WithCancel(ctx)
	var wrote sync.Once
	written := false
	var writtenMu sync.Mutex
	attemptCtx = httptrace.WithClientTrace(attemptCtx, &httptrace.ClientTrace{
		WroteHeaders: func() {
			wrote.Do(func() {
				writtenMu.Lock()
				written = true
				writtenMu.Unlock()
			})
		},
	})
	wasWritten := func() bool {
		writtenMu.Lock()
		defer writtenMu.Unlock()
		return written
	}
	httpReq, err := http.NewRequestWithContext(attemptCtx, req.method, req.url, body)
	if err != nil {
		cancel()
		return attemptOutcome{kind: outcomeNotSent, detail: "INVALID_REQUEST"}
	}
	// net/http silently resends a request with a replayable body (or an
	// Idempotency-Key header) when a reused connection fails; every attempt
	// is the runtime's to count and to decide, so the body is not
	// replayable.
	httpReq.GetBody = nil
	// A POST, PUT or PATCH without a body says so (content-length: 0, RFC
	// 9110 8.6), as fetch and httpx do: net/http writes it for these
	// methods.
	for _, h := range req.headers {
		httpReq.Header.Add(h.name, headerValueBytes(h.value))
	}
	if contentType != "" {
		httpReq.Header.Set("Content-Type", contentType)
	}
	// Without a declared media type the request accepts anything, as the
	// HTTP clients of the other runtimes (fetch, httpx, reqwest) send it.
	if len(httpReq.Header.Values("Accept")) == 0 {
		httpReq.Header.Set("Accept", "*/*")
	}
	var mu sync.Mutex
	timedOut := false
	timer := time.AfterFunc(req.timeout, func() {
		mu.Lock()
		timedOut = true
		mu.Unlock()
		cancel()
	})
	expired := func() bool {
		mu.Lock()
		defer mu.Unlock()
		return timedOut
	}
	resp, err := client.Do(httpReq)
	if err != nil {
		timer.Stop()
		cancel()
		switch {
		case expired():
			return attemptOutcome{kind: outcomeTimeout}
		case ctx.Err() != nil:
			return attemptOutcome{kind: outcomeAborted, sent: wasWritten()}
		case notSentError(err):
			return attemptOutcome{kind: outcomeNotSent, detail: failureDetail(err, false)}
		}
		var netErr net.Error
		if errors.As(err, &netErr) && netErr.Timeout() {
			return attemptOutcome{kind: outcomeTimeout}
		}
		return attemptOutcome{kind: outcomeLost, detail: failureDetail(err, true)}
	}
	headers := map[string]string{}
	for name, values := range resp.Header {
		lower := strings.ToLower(name)
		for _, value := range values {
			if known, ok := headers[lower]; ok && lower != "set-cookie" {
				headers[lower] = known + ", " + value
			} else {
				headers[lower] = value
			}
		}
	}
	status := resp.StatusCode
	if req.stream && status >= 200 && status <= 299 && isEventStream(headers) {
		timer.Stop()
		if expired() {
			_ = resp.Body.Close()
			cancel()
			return attemptOutcome{kind: outcomeTimeout}
		}
		return attemptOutcome{kind: outcomeStreaming, status: status, headers: headers,
			stream: &eventBody{response: resp, cancel: cancel, idle: req.timeout}}
	}
	data, readErr := io.ReadAll(resp.Body)
	timer.Stop()
	_ = resp.Body.Close()
	cancel()
	out := attemptOutcome{kind: outcomeResponse, status: status, headers: headers}
	switch {
	case readErr == nil:
		out.body, out.bodyOK = data, true
	case expired():
		out.bodyFailure = "timeout"
	default:
		out.bodyFailure = "broken"
	}
	return out
}

// -------------------------------------------------------------- retry-after

func daysFromCivil(year, month, day int64) int64 {
	y := year
	if month <= 2 {
		y = year - 1
	}
	var era int64
	if y >= 0 {
		era = y / 400
	} else {
		era = (y - 399) / 400
	}
	yoe := y - era*400
	mp := (month + 9) % 12
	doy := (153*mp+2)/5 + day - 1
	doe := yoe*365 + yoe/4 - yoe/100 + doy
	return era*146097 + doe - 719468
}

var monthNames = []string{"jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"}
var monthFull = []string{"january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"}

func monthNumber(token string) (int64, bool) {
	lower := strings.ToLower(token)
	if len(lower) < 3 {
		return 0, false
	}
	for i, m := range monthNames {
		if lower[:3] == m && (len(lower) == 3 || lower == monthFull[i] || lower == "sept") {
			return int64(i) + 1, true
		}
	}
	return 0, false
}

func parseDigits(s string) (int64, bool) {
	if s == "" {
		return 0, false
	}
	n, err := strconv.ParseInt(s, 10, 64)
	return n, err == nil
}

func parseClock(token string) (int64, bool) {
	parts := strings.Split(token, ":")
	if len(parts) < 2 || len(parts) > 3 {
		return 0, false
	}
	hour, ok1 := parseDigits(parts[0])
	minute, ok2 := parseDigits(parts[1])
	if !ok1 || !ok2 || hour < 0 || hour >= 24 || minute < 0 || minute >= 60 {
		return 0, false
	}
	second := 0.0
	if len(parts) == 3 {
		f, err := strconv.ParseFloat(parts[2], 64)
		if err != nil || f < 0 || f >= 61 {
			return 0, false
		}
		second = f
	}
	return (hour*3600+minute*60)*1000 + int64(second*1000+0.5), true
}

func parseZone(token string) (int64, bool) {
	switch strings.ToUpper(token) {
	case "GMT", "UTC", "UT", "Z":
		return 0, true
	}
	if token == "" {
		return 0, false
	}
	var sign int64
	switch token[0] {
	case '+':
		sign = 1
	case '-':
		sign = -1
	default:
		return 0, false
	}
	digits := strings.ReplaceAll(token[1:], ":", "")
	if len(digits) != 4 {
		return 0, false
	}
	hours, ok1 := parseDigits(digits[:2])
	minutes, ok2 := parseDigits(digits[2:])
	if !ok1 || !ok2 {
		return 0, false
	}
	return sign * (hours*60 + minutes) * 60000, true
}

func parseISODate(text string) (int64, bool) {
	date, rest, hasRest := text, "", false
	if i := strings.IndexAny(text, "T "); i >= 0 {
		date, rest, hasRest = text[:i], text[i+1:], true
	}
	fields := strings.Split(date, "-")
	if len(fields) != 3 {
		return 0, false
	}
	year, ok1 := parseDigits(fields[0])
	month, ok2 := parseDigits(fields[1])
	day, ok3 := parseDigits(fields[2])
	if !ok1 || !ok2 || !ok3 || month < 1 || month > 12 || day < 1 || day > 31 || year < 0 || year > 9999 {
		return 0, false
	}
	var clock, zone int64
	if hasRest {
		split := strings.IndexAny(rest, "Zz+-")
		if split < 0 {
			split = len(rest)
		}
		c, ok := parseClock(rest[:split])
		if !ok {
			return 0, false
		}
		clock = c
		if split < len(rest) {
			z, ok := parseZone(rest[split:])
			if !ok {
				return 0, false
			}
			zone = z
		}
	}
	return daysFromCivil(year, month, day)*86400000 + clock - zone, true
}

func allDigits(s string) bool {
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

// parseHTTPDate reads an HTTP date (IMF-fixdate, RFC 850, asctime) or an
// ISO 8601 timestamp as epoch milliseconds.
func parseHTTPDate(text string) (int64, bool) {
	text = strings.TrimSpace(text)
	if len(text) >= 8 && text[0] >= '0' && text[0] <= '9' && text[4] == '-' {
		return parseISODate(text)
	}
	var day, month, year, clock int64
	var hasDay, hasMonth, hasYear bool
	var zone int64
	for _, token := range strings.FieldsFunc(text, func(r rune) bool { return r == ' ' || r == ',' }) {
		switch {
		case strings.Contains(token, ":"):
			c, ok := parseClock(token)
			if !ok {
				return 0, false
			}
			clock = c
		case strings.Contains(token, "-") && token[0] >= '0' && token[0] <= '9':
			parts := strings.Split(token, "-")
			if len(parts) < 3 {
				return 0, false
			}
			d, ok := parseDigits(parts[0])
			if !ok {
				return 0, false
			}
			m, ok := monthNumber(parts[1])
			if !ok {
				return 0, false
			}
			y, ok := parseDigits(parts[2])
			if !ok {
				return 0, false
			}
			switch {
			case y < 50:
				y += 2000
			case y < 100:
				y += 1900
			}
			day, month, year, hasDay, hasMonth, hasYear = d, m, y, true, true, true
		case allDigits(token):
			n, _ := parseDigits(token)
			if !hasDay && len(token) <= 2 {
				day, hasDay = n, true
			} else {
				year, hasYear = n, true
			}
		default:
			if m, ok := monthNumber(token); ok {
				month, hasMonth = m, true
			} else if z, ok := parseZone(token); ok {
				zone = z
			} else {
				for _, r := range token {
					if !((r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') || r == '.') {
						return 0, false
					}
				}
			}
		}
	}
	if !hasDay || !hasMonth || !hasYear || day < 1 || day > 31 || year < 0 || year > 9999 {
		return 0, false
	}
	return daysFromCivil(year, month, day)*86400000 + clock - zone, true
}

// parseRetryAfter is Retry-After in milliseconds (delta seconds or HTTP
// date).
func parseRetryAfter(value string, present bool, nowMs int64) *int64 {
	if !present {
		return nil
	}
	trimmed := strings.TrimSpace(value)
	if allDigits(trimmed) {
		n, err := strconv.ParseUint(trimmed, 10, 64)
		if err != nil {
			return nil
		}
		ms := int64(n * 1000)
		if n > uint64(1<<62)/1000 {
			ms = 1 << 62
		}
		return &ms
	}
	date, ok := parseHTTPDate(trimmed)
	if !ok {
		return nil
	}
	delta := date - nowMs
	if delta < 0 {
		delta = 0
	}
	return &delta
}

var (
	relNext    = regexp.MustCompile(`(?i);\s*rel\s*=\s*"?([^";]*\s)?next(\s[^";]*)?"?\s*(;|$)`)
	linkTarget = regexp.MustCompile(`(?s)^\s*<([^>]*)>(.*)$`)
)

// nextLink is the next page URL of a Link header (rel="next").
func nextLink(header string) (string, bool) {
	if header == "" {
		return "", false
	}
	var parts []string
	start := 0
	for i := 0; i < len(header); i++ {
		if header[i] == ',' && strings.HasPrefix(strings.TrimLeft(header[i+1:], " \t\r\n"), "<") {
			parts = append(parts, header[start:i])
			start = i + 1
		}
	}
	parts = append(parts, header[start:])
	for _, part := range parts {
		m := linkTarget.FindStringSubmatch(part)
		if m == nil {
			continue
		}
		if relNext.MatchString(m[2]) {
			return m[1], true
		}
	}
	return "", false
}
