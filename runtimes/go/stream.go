// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"context"
	"fmt"
	"strings"
	"time"
)

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
	Meta  ResponseMeta
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
}

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

// Close closes the connection.
func (s *EventStream) Close() {
	if s.open != nil {
		s.open.body.close()
		s.open = nil
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
			open, err := s.core.openStream(ctx, s.op, s.spec, s.args, s.opts)
			if err != nil {
				return s.fail(err)
			}
			s.open, s.state = open, 1
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
			}
			s.event = item
			return true
		}
		if o.oversize {
			return s.fail(s.core.oversize(s.op, o))
		}
		if o.ended {
			s.Close()
			return false
		}
		chunk, kind := o.body.read()
		switch kind {
		case readChunk:
			o.queue = append(o.queue, o.parser.push(o.decoder.decode(chunk))...)
			o.oversize = o.parser.exceeded
		case readEnd:
			events := o.parser.push(o.decoder.finish())
			events = append(events, o.parser.end()...)
			o.queue = append(o.queue, events...)
			o.oversize = o.parser.exceeded
			o.ended = true
		case readTimeout:
			return s.fail(s.core.interrupted(s.op, o, true))
		default:
			return s.fail(s.core.interrupted(s.op, o, false))
		}
	}
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

func (c *ClientCore) openStream(ctx context.Context, op *OperationDescriptor, spec *StreamDescriptor, args any, opts CallOptions) (*openStream, *Error) {
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
		timeout: c.attemptTimeout(streamOpts),
	}, nil
}

func plural(n int, one, many string) string {
	if n == 1 {
		return one
	}
	return many
}

func (c *ClientCore) interrupted(op *OperationDescriptor, o *openStream, timedOut bool) *Error {
	after := fmt.Sprintf("after %d %s", o.count, plural(o.count, "event", "events"))
	status := o.meta.Status
	fields := unknownFields{httpStatus: &status, requestID: o.meta.RequestID}
	cctx := &callContext{api: c.api, op: op, key: o.key, hasKey: o.hasKey, keyHeader: o.keyHeader, attempts: o.meta.Attempts, check: o.check}
	var d *Diagnostic
	if timedOut {
		cause := fmt.Sprintf("No event arrived within %d ms %s; the stream was abandoned.", o.timeout.Milliseconds(), after)
		if isMutation(op) {
			d = outcomeUnknown(cctx, cause, fields)
		} else {
			d = newDiag(op.ID, UpstreamUnavailable).status(status).requestID(o.meta.RequestID).
				remedy(cause + " This read has no side effects; call again later or with a larger `Timeout`.").
				attempts(o.meta.Attempts).build()
		}
	} else {
		cause := fmt.Sprintf("The event stream was cut off %s: the connection failed before it ended.", after)
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
	out := &StreamEvent{Value: value, Event: ev.event, Meta: o.meta}
	if ev.hasID {
		id := ev.id
		out.ID = &id
	}
	if ev.hasRetry {
		r := ev.retry
		out.Retry = &r
	}
	return out, false, nil
}
