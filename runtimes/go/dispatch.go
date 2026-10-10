// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
)

// Dispatch is implemented by every generated client. It lets drivers that
// only know operation ids and JSON (the contract suite, tool servers) run
// the same typed methods as application code: Invoke decodes the arguments
// into the operation's request struct, calls the typed method and returns
// its result with the decoded wire body as Value. When the arguments do
// not decode, Invoke calls ClientCore.Call with them, so the
// VALIDATION_FAILED envelope is the pre-flight one. Arguments are keyed by
// argument name (ParamDescriptor.Name, MergedField.Arg).
type Dispatch interface {
	Core() *ClientCore
	// Operations lists every callable operation, in IR order.
	Operations() []*OperationDescriptor
	// Macros lists every macro, in IR order.
	Macros() []*MacroDescriptor
	Invoke(ctx context.Context, operation string, args any, opts CallOptions) (*Outcome, error)
	PreviewOperation(ctx context.Context, operation string, args any, opts CallOptions) (*Response[*PreviewResult], error)
	// Paginate collects every page of a paginated operation; a failure ends
	// the list with its error.
	Paginate(ctx context.Context, operation string, args any, opts CallOptions) ([]*Response[Page[any]], error)
	// StreamEvents collects the events of an operation's event stream; a
	// failure ends the list with its error.
	StreamEvents(ctx context.Context, operation string, args any, opts CallOptions) ([]StreamEvent, error)
	RunMacro(ctx context.Context, name string, input any, opts CallOptions) (*Outcome, error)
	PreviewMacro(ctx context.Context, name string, input any, opts CallOptions) (*Response[*PreviewResult], error)
}

// NoStream is VALIDATION_FAILED for an operation without an event stream.
func NoStream(operation string) error {
	return newDiag(operation, ValidationFailed).param("operation").
		expected("an operation with an event stream").
		remedy(fmt.Sprintf("%s has no event stream (its success response is not text/event-stream); use Invoke instead. Nothing was sent.", operation)).err()
}

// UnknownOperation is VALIDATION_FAILED for an operation id the client does
// not have.
func UnknownOperation(operation string) error {
	return newDiag(operation, ValidationFailed).param("operation").
		expected("an operation id of this SDK").
		remedy(fmt.Sprintf("This SDK has no operation %s; check the id (Operations() lists them). Nothing was sent.", operation)).err()
}

// UnknownMacro is VALIDATION_FAILED for a macro name the client does not
// have.
func UnknownMacro(name string) error {
	return newDiag(name, ValidationFailed).param("macro").
		expected("a macro name of this SDK").
		remedy(fmt.Sprintf("This SDK has no macro %s; check the name (Macros() lists them). Nothing was sent.", name)).err()
}

// NotPaginated is VALIDATION_FAILED for an operation without pagination.
func NotPaginated(operation string) error {
	return newDiag(operation, ValidationFailed).param("operation").
		expected("a paginated operation").
		remedy(fmt.Sprintf("%s is not paginated; use Invoke instead. Nothing was sent.", operation)).err()
}

// mismatch is why a value did not decode into a Go type.
type mismatch struct {
	path     string
	segments []string
	message  string
}

func decodeMismatch(value any, out any) *mismatch {
	err := json.Unmarshal([]byte(JSONText(value)), out)
	if err == nil {
		return nil
	}
	var typeErr *json.UnmarshalTypeError
	if errors.As(err, &typeErr) && typeErr.Field != "" {
		segments := strings.Split(typeErr.Field, ".")
		path := ""
		for _, s := range segments {
			if isIndex(s) {
				path += "[" + s + "]"
			} else {
				path += "." + s
			}
		}
		return &mismatch{path: path, segments: segments, message: fmt.Sprintf("expected %s, found a JSON %s", typeErr.Type, typeErr.Value)}
	}
	return &mismatch{message: strings.TrimPrefix(err.Error(), "json: ")}
}

func mismatchError(operation, prefix string, m *mismatch, root any, meta ResponseMeta) *Error {
	var shown any
	if len(m.segments) == 0 && prefix == "" {
		shown = envelopeValue(root, false)
	} else {
		found, _ := getPath(root, true, m.segments)
		sensitive := false
		for _, s := range m.segments {
			if looksSensitive(s) {
				sensitive = true
			}
		}
		shown = envelopeValue(found, sensitive)
	}
	location := "response" + prefix + m.path
	b := newDiag(operation, UnexpectedResponse)
	if meta.Status > 0 {
		b.status(meta.Status)
	}
	return &Error{
		Diagnostic: b.requestID(meta.RequestID).param(location).received(shown).expected(m.message).retry(RetryNever).
			remedy(fmt.Sprintf("The response of %s could not be decoded into the SDK's type at %s (%s). If the call changed state it already took effect; do not repeat it.", operation, location, m.message)).
			attempts(meta.Attempts).build(),
		Partial:    root,
		HasPartial: true,
	}
}

// Decode turns a dynamic outcome into a typed one. A missing body decodes
// as JSON null. A body that does not decode is UNEXPECTED_RESPONSE, with
// the body as Partial.
func Decode[T any](operation string, outcome *Outcome, err error) (*Response[T], error) {
	if err != nil {
		return nil, err
	}
	var value T
	if m := decodeMismatch(outcome.Value, &value); m != nil {
		return nil, mismatchError(operation, "", m, outcome.Value, outcome.Meta)
	}
	return &Response[T]{Value: value, HasValue: outcome.HasValue, Raw: outcome.Value, Meta: outcome.Meta, Verification: outcome.Verification}, nil
}

// DecodePage types the items of a page the same way.
func DecodePage[T any](operation string, page *Response[Page[any]]) (*Response[Page[T]], error) {
	items := make([]T, len(page.Value.Items))
	for i, item := range page.Value.Items {
		if m := decodeMismatch(item, &items[i]); m != nil {
			return nil, mismatchError(operation, fmt.Sprintf(".items[%d]", i), m, item, page.Meta)
		}
	}
	return &Response[Page[T]]{
		Value:    Page[T]{Items: items, Body: page.Value.Body, Next: page.Value.Next},
		HasValue: true, Raw: page.Raw, Meta: page.Meta, Verification: page.Verification,
	}, nil
}

// DecodeEvent types the value of the event at index of a stream.
func DecodeEvent[T any](operation string, index int, event *StreamEvent) (T, error) {
	var value T
	m := decodeMismatch(event.Value, &value)
	if m == nil {
		return value, nil
	}
	location := fmt.Sprintf("events[%d]%s", index, m.path)
	found, _ := getPath(event.Value, true, m.segments)
	sensitive := false
	for _, s := range m.segments {
		if looksSensitive(s) {
			sensitive = true
		}
	}
	b := newDiag(operation, UnexpectedResponse)
	if event.Meta.Status > 0 {
		b.status(event.Meta.Status)
	}
	return value, b.requestID(event.Meta.RequestID).param(location).received(envelopeValue(found, sensitive)).
		expected(m.message).retry(RetryNever).
		remedy(fmt.Sprintf("Event %d of the stream of %s could not be decoded into the SDK's type at %s (%s). If the call changed state it already took effect; do not repeat it.", index, operation, location, m.message)).
		attempts(event.Meta.Attempts).err()
}

// Find returns the descriptor with the id.
func Find(ops []*OperationDescriptor, id string) *OperationDescriptor {
	for _, op := range ops {
		if op.ID == id {
			return op
		}
	}
	return nil
}

// FindMacro returns the macro with the name.
func FindMacro(macros []*MacroDescriptor, name string) *MacroDescriptor {
	for _, m := range macros {
		if m.Name == name {
			return m
		}
	}
	return nil
}

// DecodeArgs decodes an arguments object into a request struct, refusing
// unknown members; false when it does not decode (the caller then calls
// the core with the raw arguments, so pre-flight validation reports it).
func DecodeArgs(args any, out any) bool {
	converted, err := FromGo(args)
	if err != nil {
		return false
	}
	if converted == nil {
		converted = NewObject()
	}
	dec := json.NewDecoder(strings.NewReader(JSONText(converted)))
	dec.DisallowUnknownFields()
	return dec.Decode(out) == nil
}
