// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
)

// BinaryKey is the member that marks bytes inside an arguments object
// (which is JSON): {"$tungsten_binary": "<base64>", "filename": ...,
// "content_type": ...}. Only the request encoder interprets it.
const BinaryKey = "$tungsten_binary"

// Binary is the value of a bytes or multipart body field: the bytes, and
// for a multipart part its optional file name and media type. Base64 text
// inside JSON models (format: byte) is a plain []byte field instead.
type Binary struct {
	Data        []byte
	Filename    string
	ContentType string
}

// NewBinary wraps bytes.
func NewBinary(data []byte) Binary {
	return Binary{Data: data}
}

// WithFilename sets the multipart file name.
func (b Binary) WithFilename(name string) Binary {
	b.Filename = name
	return b
}

// WithContentType sets the multipart part's media type.
func (b Binary) WithContentType(media string) Binary {
	b.ContentType = media
	return b
}

// String never shows the bytes.
func (b Binary) String() string {
	return fmt.Sprintf("Binary(%d bytes)", len(b.Data))
}

// MarshalJSON writes the tagged object.
func (b Binary) MarshalJSON() ([]byte, error) {
	return []byte(JSONText(b.object())), nil
}

func (b Binary) object() *Object {
	o := NewObject()
	o.Set(BinaryKey, base64.StdEncoding.EncodeToString(b.Data))
	if b.Filename != "" {
		o.Set("filename", b.Filename)
	}
	if b.ContentType != "" {
		o.Set("content_type", b.ContentType)
	}
	return o
}

// UnmarshalJSON reads the tagged object.
func (b *Binary) UnmarshalJSON(data []byte) error {
	v, err := ParseJSON(data)
	if err != nil {
		return err
	}
	bin, ok := BinaryOf(v)
	if !ok {
		return errors.New(`a binary value ({"$tungsten_binary": "<base64>"})`)
	}
	*b = bin
	return nil
}

// BinaryOf returns the Binary a tagged object stands for.
func BinaryOf(v any) (Binary, bool) {
	o, ok := v.(*Object)
	if !ok || !o.Has(BinaryKey) {
		return Binary{}, false
	}
	var out Binary
	for _, k := range o.keys {
		val := o.vals[k]
		switch k {
		case BinaryKey:
			text, ok := val.(string)
			if !ok {
				return Binary{}, false
			}
			data, err := base64.StdEncoding.DecodeString(text)
			if err != nil {
				return Binary{}, false
			}
			out.Data = data
		case "filename":
			if val == nil {
				continue
			}
			text, ok := val.(string)
			if !ok {
				return Binary{}, false
			}
			out.Filename = text
		case "content_type":
			if val == nil {
				continue
			}
			text, ok := val.(string)
			if !ok {
				return Binary{}, false
			}
			out.ContentType = text
		default:
			return Binary{}, false
		}
	}
	return out, true
}

// Patch is a field that may be absent, null or a value (presence
// optional-nullable). Use it as *Patch[T] with omitempty: a nil pointer is
// absent, Null[T]() is null, Some(v) is the value.
type Patch[T any] struct {
	// IsNull is true for an explicit null.
	IsNull bool
	Value  T
}

// Null returns an explicit null.
func Null[T any]() *Patch[T] {
	return &Patch[T]{IsNull: true}
}

// Some returns a present value.
func Some[T any](v T) *Patch[T] {
	return &Patch[T]{Value: v}
}

// Get returns the value and whether it is present and not null.
func (p *Patch[T]) Get() (T, bool) {
	if p == nil || p.IsNull {
		var zero T
		return zero, false
	}
	return p.Value, true
}

// MarshalJSON writes null or the value.
func (p Patch[T]) MarshalJSON() ([]byte, error) {
	if p.IsNull {
		return []byte("null"), nil
	}
	return json.Marshal(p.Value)
}

// UnmarshalJSON reads null or the value.
func (p *Patch[T]) UnmarshalJSON(data []byte) error {
	if string(data) == "null" {
		*p = Patch[T]{IsNull: true}
		return nil
	}
	var v T
	if err := json.Unmarshal(data, &v); err != nil {
		return err
	}
	*p = Patch[T]{Value: v}
	return nil
}

// Ptr returns a pointer to v, for optional fields.
func Ptr[T any](v T) *T {
	return &v
}
