// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"context"
	"fmt"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

// defaultReconnectMs is the wait before a reconnect when the server sent no
// retry value.
const defaultReconnectMs = 250

// StreamEvent is one event of a stream.
type StreamEvent struct {
	// Value is the event's data, decoded from JSON.
	Value any
	// Event is the event field, "message" when the event has none.
	Event string
	// ID is the last event id the stream has set so far (nil before any).
	ID *string
	// Retry is the last retry value (milliseconds) set so far.
	Retry *int64
	// Meta is the answer of the connection the event arrived on.
	Meta ResponseMeta
	// Reconnects is how often the stream reconnected up to this event.
	Reconnects int
}

type openStream struct {
	body      *eventBody
	parser    *sseParser
	decoder   utf8Decoder
	queue     []sseEvent
	ended     bool
	oversize  bool
	meta      ResponseMeta
	count     int
	key       string
	hasKey    bool
	keyHeader string
	check     *outcomeCheck
	secrets   secretSet
	timeout   time.Duration
	// repeatable: the call is safe to repeat (a read, or a mutation with
	// replay protection).
	repeatable bool
	reconnects int
	lastID     *string
	lastRetry  *int64
	// delivered are the ids delivered so far; after a reconnect (replaying)
	// events with one of them are skipped until the first new one, so a
	// server that ignores Last-Event-ID never delivers an event twice.
	delivered map[string]bool
	replaying bool
}

// dropped is why a connection ended without finishing the stream.
type dropped int

const (
	droppedNone dropped = iota
	droppedLost
	droppedIdle
	droppedEarlyEnd
)

// EventStream iterates the events of an operation's event stream, like
// bufio.Scanner: the request is sent by the first Next; an error ends the
// iteration (Err). Close releases the connection; leaving the loop early
// must call it.
type EventStream struct {
	core  *ClientCore
	op    *OperationDescriptor
	spec  *StreamDescriptor
	args  any
	opts  CallOptions
	open  *openStream
	state int // 0 idle, 1 open, 2 done
	event *StreamEvent
	err   *Error
	// cancelled is set by Cancel, from any goroutine; mu guards body, the
	// connection Cancel closes.
	cancelled atomic.Bool
	mu        sync.Mutex
	body      *eventBody
}

// setOpen makes o the current connection (nil: none).
func (s *EventStream) setOpen(o *openStream) {
	s.mu.Lock()
	s.open = o
	s.body = nil
	if o != nil {
		s.body = o.body
	}
	cancelled := s.cancelled.Load()
	s.mu.Unlock()
	if cancelled && o != nil {
		o.body.close()
	}
}

// Stream returns an iterator over the server-sent events of op; spec is
// the stream's descriptor.
func (c *ClientCore) Stream(op *OperationDescriptor, spec *StreamDescriptor, args any, opts CallOptions) *EventStream {
	if spec == nil {
		spec = &StreamDescriptor{}
	}
	return &EventStream{core: c, op: op, spec: spec, args: args, opts: opts}
}

// Event is the event Next delivered.
func (s *EventStream) Event() *StreamEvent { return s.event }

// Err is the error that ended the stream, or nil.
func (s *EventStream) Err() error {
	if s.err == nil {
		return nil
	}
	return s.err
}

// Cancel ends the stream for good, without an error and without a
// reconnect; it may be called from another goroutine to end a blocked Next.
func (s *EventStream) Cancel() {
	if s.cancelled.CompareAndSwap(false, true) {
		s.mu.Lock()
		body := s.body
		s.mu.Unlock()
		if body != nil {
			body.close()
		}
	}
}

// Close closes the connection.
func (s *EventStream) Close() {
	if s.open != nil {
		s.open.body.close()
		s.setOpen(nil)
	}
	s.state = 2
}

func (s *EventStream) fail(err *Error) bool {
	s.err = err
	s.Close()
	return false
}

// Next delivers the next event; false at the end of the stream or after an
// error.
func (s *EventStream) Next(ctx context.Context) bool {
	s.event = nil
	for {
		switch s.state {
		case 2:
			return false
		case 0:
			if s.cancelled.Load() {
				s.state = 2
				return false
			}
			open, err := s.core.openStream(ctx, s.op, s.spec, s.args, s.opts, nil)
			if err != nil {
				return s.fail(err)
			}
			s.setOpen(open)
			s.state = 1
			if s.cancelled.Load() {
				s.Close()
				return false
			}
			continue
		}
		o := s.open
		if len(o.queue) > 0 {
			ev := o.queue[0]
			o.queue = o.queue[1:]
			item, done, err := s.core.accept(s.op, s.spec, o, ev)
			switch {
			case err != nil:
				return s.fail(err)
			case done:
				s.Close()
				return false
			case item == nil:
				continue
			}
			s.event = item
			return true
		}
		if o.oversize {
			return s.fail(s.core.oversize(s.op, o))
		}
		why := droppedNone
		if o.ended {
			// A stream that names its terminal event has not finished without it.
			if s.spec.Done == nil {
				s.Close()
				return false
			}
			why = droppedEarlyEnd
		} else {
			chunk, kind := o.body.read()
			if s.cancelled.Load() {
				s.Close()
				return false
			}
			switch kind {
			case readChunk:
				o.queue = append(o.queue, o.parser.push(o.decoder.decode(chunk))...)
				o.oversize = o.parser.exceeded
				continue
			case readEnd:
				events := o.parser.push(o.decoder.finish())
				events = append(events, o.parser.end()...)
				o.queue = append(o.queue, events...)
				o.oversize = o.parser.exceeded
				o.ended = true
				continue
			case readTimeout:
				if s.core.idleTimeout <= 0 {
					return s.fail(s.core.interrupted(s.op, o, droppedNone))
				}
				why = droppedIdle
			default:
				why = droppedLost
			}
		}
		// The caller's context ends the stream for good.
		if ctx.Err() != nil || !s.core.mayReconnect(o) {
			// Without reconnects a clean end stays a clean end.
			if why == droppedEarlyEnd && o.reconnects == 0 {
				s.Close()
				return false
			}
			return s.fail(s.core.interrupted(s.op, o, why))
		}
		fresh, err := s.core.reconnect(ctx, s.op, s.spec, s.args, s.opts, o)
		if s.cancelled.Load() {
			if fresh != nil {
				fresh.body.close()
			}
			s.Close()
			return false
		}
		if err != nil {
			return s.fail(err)
		}
		s.setOpen(fresh)
	}
}

// Folded is what the consuming helpers of a stream return: the result, or,
// when the stream failed, what was gathered before the failure with the
// failure as Err.
type Folded[R any] struct {
	Value R
	Err   error
	// Reconnects of the last event delivered.
	Reconnects int
}

// EventHandlers are handlers by event name for On: the one named like the
// event ("message" for an event without a name) is called, else the one
// named "*".
type EventHandlers map[string]func(*StreamEvent)

// Reduce reads the stream to its end and folds its events into one value
// (concatenating text deltas, say), then closes it.
func Reduce[A any](ctx context.Context, s *EventStream, initial A, fold func(A, *StreamEvent) A) Folded[A] {
	defer s.Close()
	out := Folded[A]{Value: initial}
	for s.Next(ctx) {
		out.Reconnects = s.event.Reconnects
		out.Value = fold(out.Value, s.event)
	}
	out.Err = s.Err()
	return out
}

// First is the first event for which predicate holds, then closes the
// stream; Value is nil when the stream ends without one.
func (s *EventStream) First(ctx context.Context, predicate func(*StreamEvent) bool) Folded[*StreamEvent] {
	defer s.Close()
	var out Folded[*StreamEvent]
	for s.Next(ctx) {
		out.Reconnects = s.event.Reconnects
		if predicate(s.event) {
			out.Value = s.event
			return out
		}
	}
	out.Err = s.Err()
	return out
}

// On reads the stream to its end and calls the handler named like each
// event; Value is the number of events delivered.
func (s *EventStream) On(ctx context.Context, handlers EventHandlers) Folded[int] {
	defer s.Close()
	var out Folded[int]
	for s.Next(ctx) {
		out.Reconnects = s.event.Reconnects
		out.Value++
		if h, ok := handlers[s.event.Event]; ok && h != nil {
			h(s.event)
		} else if h, ok := handlers["*"]; ok && h != nil {
			h(s.event)
		}
	}
	out.Err = s.Err()
	return out
}

// CollectValues returns every event in order and the final error, if the
// stream failed. The collection is bounded: it ends with an
// UNEXPECTED_RESPONSE error once the kept events hold more than
// ClientOptions.MaxCollectBytes of JSON or it ran longer than
// MaxCollectTime.
func (s *EventStream) CollectValues(ctx context.Context) ([]StreamEvent, error) {
	limitCtx, cancel := context.WithTimeout(ctx, s.core.maxCollectTime)
	defer cancel()
	deadline := time.Now().Add(s.core.maxCollectTime)
	var out []StreamEvent
	bytes := 0
	for s.Next(limitCtx) {
		if time.Now().After(deadline) {
			s.Close()
			return out, s.core.overBudget(s.op, false, len(out))
		}
		ev := *s.event
		bytes += len(JSONText(ev.Value))
		if bytes > s.core.maxCollectBytes {
			s.Close()
			return out, s.core.overBudget(s.op, true, len(out))
		}
		out = append(out, ev)
	}
	if limitCtx.Err() != nil && ctx.Err() == nil {
		return out, s.core.overBudget(s.op, false, len(out))
	}
	return out, s.Err()
}

// withStreamFlag sets the stream's request flag to true, in a merged body
// field or inside an object body argument.
func withStreamFlag(op *OperationDescriptor, spec *StreamDescriptor, args any) any {
	o, ok := args.(*Object)
	if spec.Flag == "" || op.Body == nil || !ok {
		return args
	}
	out := o.Clone()
	if op.Body.Arg == "" {
		for _, f := range op.Body.Fields {
			if f.Wire == spec.Flag {
				out.Set(f.Arg, true)
				break
			}
		}
	} else if inner, ok := o.vals[op.Body.Arg].(*Object); ok {
		copied := inner.Clone()
		copied.Set(spec.Flag, true)
		out.Set(op.Body.Arg, copied)
	}
	return out
}

func (c *ClientCore) openStream(ctx context.Context, op *OperationDescriptor, spec *StreamDescriptor, args any, opts CallOptions, resumeFrom *string) (*openStream, *Error) {
	headers := map[string]string{}
	accept := ""
	for _, layer := range []struct {
		own bool
		m   map[string]string
	}{{false, c.headers}, {true, opts.Headers}} {
		for name, value := range layer.m {
			if strings.EqualFold(name, "accept") {
				accept = value
			} else if layer.own {
				headers[name] = value
			}
		}
	}
	headers["Accept"] = mergeAccept(accept)
	if resumeFrom != nil {
		for name := range headers {
			if strings.EqualFold(name, "last-event-id") {
				delete(headers, name)
			}
		}
		headers["Last-Event-ID"] = *resumeFrom
	}
	streamOpts := opts
	streamOpts.Headers = headers
	if converted, err := FromGo(args); err == nil {
		args = converted
	}
	p, err := c.prepare(ctx, op, withStreamFlag(op, spec, args), streamOpts, purposeCall, "", nil)
	if err != nil {
		return nil, err
	}
	result, err := c.sendWith(ctx, p, streamOpts, true)
	if err != nil {
		return nil, err
	}
	if result.stream == nil {
		response := result.response
		contentType, hasType := response.Meta.Headers["content-type"]
		after := ""
		if isMutation(op) {
			after = " The call took effect; do not repeat it."
		}
		shownType := "missing"
		var received any
		if hasType {
			shownType = contentType
			received = envelopeValue(contentType, false)
		}
		return nil, newDiag(op.ID, UnexpectedResponse).status(response.Meta.Status).requestID(response.Meta.RequestID).
			param("response").received(received).expected("a text/event-stream body").
			remedy(fmt.Sprintf("The success response is not an event stream (Content-Type %s).%s", shownType, after)).
			retry(RetryNever).attempts(response.Meta.Attempts).err()
	}
	var check *outcomeCheck
	if isMutation(op) && !hasReplayProtection(op, p.hasKey) {
		check = c.outcomeCheck(op, p.args)
	}
	return &openStream{
		body: result.stream.body, parser: newSSEParser(c.maxEventBytes), meta: result.stream.meta,
		key: p.key, hasKey: p.hasKey, keyHeader: p.keyHeader, check: check, secrets: p.secrets.clone(),
		timeout:    c.attemptTimeout(streamOpts),
		repeatable: !isMutation(op) || hasReplayProtection(op, p.hasKey),
		delivered:  map[string]bool{},
	}, nil
}

// mayReconnect: a dropped stream may be reconnected when the call is safe
// to repeat, reconnects are left, and an event id to resume from is known
// (or no event was delivered yet).
func (c *ClientCore) mayReconnect(o *openStream) bool {
	return o.repeatable && o.reconnects < c.maxReconnects &&
		(o.count == 0 || (o.lastID != nil && validHeaderValue(*o.lastID)))
}

// reconnect waits as the server asked (capped, shortened by up to 25 % at
// random), then sends the request again with Last-Event-ID: the stream that
// continues old.
func (c *ClientCore) reconnect(ctx context.Context, op *OperationDescriptor, spec *StreamDescriptor, args any, opts CallOptions, old *openStream) (*openStream, *Error) {
	old.body.close()
	ceiling := float64(defaultReconnectMs)
	if old.lastRetry != nil {
		ceiling = float64(*old.lastRetry)
	}
	ceiling = min(ceiling, float64(c.reconnectMax.Milliseconds()))
	wait := ceiling - c.random()*0.25*ceiling
	timer := time.NewTimer(time.Duration(wait * float64(time.Millisecond)))
	select {
	case <-timer.C:
	case <-ctx.Done():
		timer.Stop()
	}
	fresh, err := c.openStream(ctx, op, spec, args, opts, old.lastID)
	if err != nil {
		return nil, err
	}
	fresh.count = old.count
	fresh.reconnects = old.reconnects + 1
	fresh.lastID, fresh.lastRetry = old.lastID, old.lastRetry
	fresh.delivered = old.delivered
	fresh.replaying = true
	return fresh, nil
}

func plural(n int, one, many string) string {
	if n == 1 {
		return one
	}
	return many
}

// interrupted is the final error of a stream that broke after o.count
// events; droppedNone is a silence past the attempt timeout.
func (c *ClientCore) interrupted(op *OperationDescriptor, o *openStream, why dropped) *Error {
	after := fmt.Sprintf("after %d %s", o.count, plural(o.count, "event", "events"))
	retried := ""
	switch o.reconnects {
	case 0:
	case 1:
		retried = " (1 reconnect made)"
	default:
		retried = fmt.Sprintf(" (%d reconnects made)", o.reconnects)
	}
	idle := why == droppedIdle
	var code *string
	if idle {
		text := "STREAM_IDLE"
		code = &text
	}
	status := o.meta.Status
	fields := unknownFields{httpStatus: &status, code: code, requestID: o.meta.RequestID}
	cctx := &callContext{api: c.api, op: op, key: o.key, hasKey: o.hasKey, keyHeader: o.keyHeader, attempts: o.meta.Attempts, check: o.check}
	var d *Diagnostic
	if why == droppedNone || idle {
		silence := fmt.Sprintf("No event arrived within %d ms %s", o.timeout.Milliseconds(), after)
		option := "`Timeout`"
		if idle {
			silence = fmt.Sprintf("No bytes arrived within %d ms %s%s", c.idleTimeout.Milliseconds(), after, retried)
			option = "`IdleTimeout`"
		}
		cause := silence + "; the stream was abandoned."
		if isMutation(op) {
			d = outcomeUnknown(cctx, cause, fields)
		} else {
			d = newDiag(op.ID, UpstreamUnavailable).status(status).code(code).requestID(o.meta.RequestID).
				remedy(cause + " This read has no side effects; call again later or with a larger " + option + ".").
				attempts(o.meta.Attempts).build()
		}
	} else {
		cause := fmt.Sprintf("The event stream was cut off %s%s: the connection failed before it ended.", after, retried)
		if isMutation(op) {
			d = outcomeUnknown(cctx, cause, fields)
		} else {
			d = newDiag(op.ID, TransportFailed).status(status).requestID(o.meta.RequestID).
				remedy(cause + " This read has no side effects; call again.").
				attempts(o.meta.Attempts).build()
		}
	}
	return newError(scrubDiagnostic(d, &o.secrets))
}

func (c *ClientCore) overBudget(op *OperationDescriptor, bytes bool, kept int) *Error {
	what := fmt.Sprintf("at most %d ms to collect the events", c.maxCollectTime.Milliseconds())
	if bytes {
		what = fmt.Sprintf("at most %d bytes of events", c.maxCollectBytes)
	}
	after := ""
	if isMutation(op) {
		after = " The call took effect; do not repeat it."
	}
	return newDiag(op.ID, UnexpectedResponse).param("events").expected(what).
		remedy(fmt.Sprintf("The stream was abandoned after %d %s (%s; ClientOptions.MaxCollectBytes and MaxCollectTime).%s The events collected so far precede this error.",
			kept, plural(kept, "event", "events"), what, after)).
		retry(RetryNever).err()
}

func (c *ClientCore) oversize(op *OperationDescriptor, o *openStream) *Error {
	limit := c.maxEventBytes
	after := ""
	if isMutation(op) {
		after = " The call took effect; do not repeat it."
	}
	return newError(scrubDiagnostic(newDiag(op.ID, UnexpectedResponse).status(o.meta.Status).requestID(o.meta.RequestID).
		param(fmt.Sprintf("events[%d]", o.count)).
		expected(fmt.Sprintf("an event of at most %d bytes", limit)).
		remedy(fmt.Sprintf("Event %d of the stream is larger than the limit of %d bytes (ClientOptions.MaxEventBytes); the stream was abandoned.%s Events before it were delivered. Raise MaxEventBytes if the server sends events this large on purpose.", o.count, limit, after)).
		retry(RetryNever).attempts(o.meta.Attempts).build(), &o.secrets))
}

// accept decodes, checks and numbers one event; done is the done sentinel.
func (c *ClientCore) accept(op *OperationDescriptor, spec *StreamDescriptor, o *openStream, ev sseEvent) (*StreamEvent, bool, *Error) {
	if spec.Done != nil && *spec.Done == ev.data {
		return nil, true, nil
	}
	if o.replaying {
		if ev.hasID && o.delivered[ev.id] {
			return nil, false, nil
		}
		o.replaying = false
	}
	index := o.count
	afterEffect := ""
	if isMutation(op) {
		afterEffect = " The call took effect; do not repeat it."
	}
	failure := func(b *diag) *Error {
		return newError(scrubDiagnostic(b.status(o.meta.Status).requestID(o.meta.RequestID).retry(RetryNever).attempts(o.meta.Attempts).build(), &o.secrets))
	}
	value, err := ParseJSONString(ev.data)
	if err != nil {
		return nil, false, failure(newDiag(op.ID, UnexpectedResponse).param(fmt.Sprintf("events[%d]", index)).
			received(envelopeValue(ev.data, false)).expected("JSON in the data of every event").
			remedy(fmt.Sprintf("Event %d of the stream (event \"%s\") does not carry JSON in its data.%s Events before it were delivered.", index, ev.event, afterEffect)))
	}
	if mode := c.validateResponses; mode != ValidateOff && spec.Event != nil {
		if issues := judge(spec.Event, value); len(issues) > 0 {
			issue := issues[0]
			path := segmentStrings(issue.Path)
			pathText := formatIssuePath(issue.Path)
			redacted := redactPaths(value, op.Agent.SensitiveResponseFields)
			received, _ := getPath(redacted, true, path)
			sensitive := false
			for _, s := range path {
				if looksSensitive(s) {
					sensitive = true
				}
			}
			e := failure(newDiag(op.ID, UnexpectedResponse).param(fmt.Sprintf("events[%d]%s", index, pathText)).
				received(envelopeValue(received, sensitive)).expected(issue.Message).
				remedy(fmt.Sprintf("Event %d of the stream (event \"%s\") does not match the API description at events[%d]%s (%s).%s", index, ev.event, index, pathText, issue.Message, afterEffect)))
			if mode == ValidateStrict {
				return nil, false, e
			}
			c.emit(e.Diagnostic)
		}
	}
	o.count++
	if ev.hasID {
		id := ev.id
		o.delivered[id] = true
		o.lastID = &id
	}
	if ev.hasRetry {
		r := ev.retry
		o.lastRetry = &r
	}
	out := &StreamEvent{Value: value, Event: ev.event, Meta: o.meta, Reconnects: o.reconnects}
	if o.lastID != nil {
		id := *o.lastID
		out.ID = &id
	}
	if o.lastRetry != nil {
		r := *o.lastRetry
		out.Retry = &r
	}
	return out, false, nil
}
