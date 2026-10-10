// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sort"
	"time"
)

// The helpers generated SDKs are written with.

// NoContent is the result type of an operation that answers without a
// body.
type NoContent struct{}

// MustJSON parses JSON text that the generator wrote; it panics on invalid
// text, which only a broken generator produces.
func MustJSON(text string) any {
	v, err := ParseJSONString(text)
	if err != nil {
		panic("tungsten: invalid generated JSON: " + err.Error())
	}
	return v
}

// MustJSONList parses a generated JSON array.
func MustJSONList(text string) []any {
	list, ok := MustJSON(text).([]any)
	if !ok {
		panic("tungsten: generated JSON is not an array")
	}
	return list
}

// MustJSONObject parses a generated JSON object.
func MustJSONObject(text string) *Object {
	o, ok := MustJSON(text).(*Object)
	if !ok {
		panic("tungsten: generated JSON is not an object")
	}
	return o
}

// Millis is a pointer to a duration of n milliseconds.
func Millis(n int64) *time.Duration {
	d := time.Duration(n) * time.Millisecond
	return &d
}

// ArgumentsError is VALIDATION_FAILED for a request struct that could not
// be encoded as JSON (a NaN, a value whose MarshalJSON failed).
func ArgumentsError(operation string, err error) error {
	return newDiag(operation, ValidationFailed).param("args").expected("arguments that encode as JSON").
		remedy(fmt.Sprintf("The arguments of %s could not be encoded as JSON (%s). Nothing was sent.", operation, err)).err()
}

// CallTyped encodes a request struct, calls the operation and decodes the
// body into T.
func CallTyped[T any](ctx context.Context, core *ClientCore, op *OperationDescriptor, request any, opts CallOptions) (*Response[T], error) {
	args, err := Marshal(request)
	if err != nil {
		return nil, ArgumentsError(op.ID, err)
	}
	out, err := core.Call(ctx, op, args, opts)
	return Decode[T](op.ID, out, err)
}

// PreviewTyped encodes a request struct and previews the operation.
func PreviewTyped(ctx context.Context, core *ClientCore, op *OperationDescriptor, request any, opts CallOptions) (*Response[*PreviewResult], error) {
	args, err := Marshal(request)
	if err != nil {
		return nil, ArgumentsError(op.ID, err)
	}
	return core.Preview(ctx, op, args, opts)
}

// Untyped turns a typed result back into a dynamic one whose Value is the
// decoded wire body.
func Untyped[T any](r *Response[T], err error) (*Outcome, error) {
	if err != nil {
		return nil, err
	}
	return &Outcome{Value: r.Raw, HasValue: r.HasValue, Raw: r.Raw, Meta: r.Meta, Verification: r.Verification}, nil
}

// TypedPages iterates the pages of an operation with items typed as T,
// like Pages; an item that does not decode ends the iteration with an
// UNEXPECTED_RESPONSE error.
type TypedPages[T any] struct {
	pages *Pages
	page  *Response[Page[T]]
	raw   *Response[Page[any]]
	err   error
}

// PagesTyped encodes a request struct and iterates the operation's pages.
func PagesTyped[T any](core *ClientCore, op *OperationDescriptor, request any, opts CallOptions) *TypedPages[T] {
	args, err := Marshal(request)
	if err != nil {
		return &TypedPages[T]{err: ArgumentsError(op.ID, err)}
	}
	return &TypedPages[T]{pages: core.Pages(op, args, opts)}
}

// Next fetches the next page; false after the last page or an error.
func (p *TypedPages[T]) Next(ctx context.Context) bool {
	p.page, p.raw = nil, nil
	if p.err != nil || p.pages == nil {
		return false
	}
	if !p.pages.Next(ctx) {
		p.err = p.pages.Err()
		return false
	}
	raw := p.pages.Page()
	typed, err := DecodePage[T](p.pages.op.ID, raw)
	if err != nil {
		p.err = err
		return false
	}
	p.page, p.raw = typed, raw
	return true
}

// Page is the page Next fetched.
func (p *TypedPages[T]) Page() *Response[Page[T]] { return p.page }

// Raw is the page Next fetched with its items as decoded JSON.
func (p *TypedPages[T]) Raw() *Response[Page[any]] { return p.raw }

// Err is the error that ended the iteration, or nil.
func (p *TypedPages[T]) Err() error { return p.err }

// All collects every typed page.
func (p *TypedPages[T]) All(ctx context.Context) ([]*Response[Page[T]], error) {
	var out []*Response[Page[T]]
	for p.Next(ctx) {
		out = append(out, p.Page())
	}
	return out, p.Err()
}

// AllRaw collects every page with its items as decoded JSON, checking that
// each decodes as T.
func (p *TypedPages[T]) AllRaw(ctx context.Context) ([]*Response[Page[any]], error) {
	var out []*Response[Page[any]]
	for p.Next(ctx) {
		out = append(out, p.Raw())
	}
	return out, p.Err()
}

// TypedEvents iterates an event stream with values typed as T; an event
// that does not decode ends the stream with an UNEXPECTED_RESPONSE error.
type TypedEvents[T any] struct {
	stream *EventStream
	value  T
	event  *StreamEvent
	seen   int
	err    error
}

// StreamTyped encodes a request struct and opens the operation's event
// stream (on the first Next).
func StreamTyped[T any](core *ClientCore, op *OperationDescriptor, spec *StreamDescriptor, request any, opts CallOptions) *TypedEvents[T] {
	args, err := Marshal(request)
	if err != nil {
		return &TypedEvents[T]{err: ArgumentsError(op.ID, err)}
	}
	return &TypedEvents[T]{stream: core.Stream(op, spec, args, opts)}
}

// Next delivers the next event; false at the end or after an error.
func (e *TypedEvents[T]) Next(ctx context.Context) bool {
	var zero T
	e.value, e.event = zero, nil
	if e.err != nil || e.stream == nil {
		return false
	}
	if !e.stream.Next(ctx) {
		e.err = e.stream.Err()
		return false
	}
	ev := e.stream.Event()
	value, err := DecodeEvent[T](e.stream.op.ID, e.seen, ev)
	e.seen++
	if err != nil {
		e.err = err
		e.stream.Close()
		return false
	}
	e.value, e.event = value, ev
	return true
}

// Value is the typed value of the event Next delivered.
func (e *TypedEvents[T]) Value() T { return e.value }

// Event is the event Next delivered, its value as decoded JSON.
func (e *TypedEvents[T]) Event() *StreamEvent { return e.event }

// Err is the error that ended the stream, or nil.
func (e *TypedEvents[T]) Err() error { return e.err }

// Close closes the connection.
func (e *TypedEvents[T]) Close() {
	if e.stream != nil {
		e.stream.Close()
	}
}

// Cancel ends the stream for good, without an error; it may be called
// from another goroutine.
func (e *TypedEvents[T]) Cancel() {
	if e.stream != nil {
		e.stream.Cancel()
	}
}

// First is the first event for which predicate holds (its typed value),
// then closes the stream; Value is nil when the stream ends without one.
func (e *TypedEvents[T]) First(ctx context.Context, predicate func(T, *StreamEvent) bool) Folded[*T] {
	defer e.Close()
	var out Folded[*T]
	for e.Next(ctx) {
		out.Reconnects = e.event.Reconnects
		if predicate(e.value, e.event) {
			v := e.value
			out.Value = &v
			return out
		}
	}
	out.Err = e.Err()
	return out
}

// On reads the stream to its end and calls the handler named like each
// event ("*" for the others) with its typed value; Value is the number of
// events delivered.
func (e *TypedEvents[T]) On(ctx context.Context, handlers map[string]func(T, *StreamEvent)) Folded[int] {
	defer e.Close()
	var out Folded[int]
	for e.Next(ctx) {
		out.Reconnects = e.event.Reconnects
		out.Value++
		if h, ok := handlers[e.event.Event]; ok && h != nil {
			h(e.value, e.event)
		} else if h, ok := handlers["*"]; ok && h != nil {
			h(e.value, e.event)
		}
	}
	out.Err = e.Err()
	return out
}

// ReduceTyped reads a typed stream to its end and folds its values into one
// value, then closes it.
func ReduceTyped[T, A any](ctx context.Context, e *TypedEvents[T], initial A, fold func(A, T, *StreamEvent) A) Folded[A] {
	defer e.Close()
	out := Folded[A]{Value: initial}
	for e.Next(ctx) {
		out.Reconnects = e.event.Reconnects
		out.Value = fold(out.Value, e.value, e.event)
	}
	out.Err = e.Err()
	return out
}

// CollectValues returns every event (each checked to decode as T, its value
// as decoded JSON) and the final error, within ClientOptions.MaxCollectBytes
// and MaxCollectTime.
func (e *TypedEvents[T]) CollectValues(ctx context.Context) ([]StreamEvent, error) {
	if e.stream == nil {
		return nil, e.err
	}
	core := e.stream.core
	limitCtx, cancel := context.WithTimeout(ctx, core.maxCollectTime)
	defer cancel()
	deadline := time.Now().Add(core.maxCollectTime)
	var out []StreamEvent
	size := 0
	for e.Next(limitCtx) {
		if time.Now().After(deadline) {
			e.Close()
			return out, core.overBudget(e.stream.op, false, len(out))
		}
		ev := *e.event
		size += len(JSONText(ev.Value))
		if size > core.maxCollectBytes {
			e.Close()
			return out, core.overBudget(e.stream.op, true, len(out))
		}
		out = append(out, ev)
	}
	if limitCtx.Err() != nil && ctx.Err() == nil {
		return out, core.overBudget(e.stream.op, false, len(out))
	}
	return out, e.Err()
}

// PickVariant is the index of the variant of a union schema that the JSON
// value matches: by its discriminator when the union is tagged, else the
// first variant that accepts the value.
func PickVariant(s *Schema, data []byte) (int, error) {
	value, err := ParseJSON(data)
	if err != nil {
		return 0, err
	}
	if value == nil {
		return -1, nil
	}
	if s.Tag != "" {
		o, ok := value.(*Object)
		if !ok {
			return 0, errors.New("expected an object with the discriminator " + s.Tag)
		}
		tag, _ := o.vals[s.Tag].(string)
		for i, t := range s.Tags {
			if t == tag {
				return i, nil
			}
		}
		return 0, fmt.Errorf("unknown %s %s", s.Tag, jsString(tag))
	}
	for i, v := range s.Variants {
		if accepts(v, value) {
			return i, nil
		}
	}
	return 0, errors.New("the value matches no variant")
}

// MarshalWithExtra writes a struct's members followed by its additional
// members (sorted by name).
func MarshalWithExtra[V any](fixed any, extra map[string]V) ([]byte, error) {
	data, err := json.Marshal(fixed)
	if err != nil {
		return nil, err
	}
	v, err := ParseJSON(data)
	if err != nil {
		return nil, err
	}
	o, ok := v.(*Object)
	if !ok {
		return data, nil
	}
	keys := make([]string, 0, len(extra))
	for k := range extra {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		if o.Has(k) {
			continue
		}
		item, err := FromGo(extra[k])
		if err != nil {
			return nil, err
		}
		o.Set(k, item)
	}
	return []byte(JSONText(o)), nil
}

// ExtraMembers decodes the members of a JSON object that are not known.
func ExtraMembers[V any](data []byte, known []string) (map[string]V, error) {
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return nil, err
	}
	skip := make(map[string]bool, len(known))
	for _, k := range known {
		skip[k] = true
	}
	var out map[string]V
	for k, v := range raw {
		if skip[k] {
			continue
		}
		var item V
		if err := json.Unmarshal(v, &item); err != nil {
			return nil, err
		}
		if out == nil {
			out = map[string]V{}
		}
		out[k] = item
	}
	return out, nil
}
