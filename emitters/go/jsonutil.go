// SPDX-License-Identifier: Apache-2.0

package main

import (
	"bytes"
	"encoding/json"
	"errors"
)

// renameKeys rewrites the member names of a JSON object, keeping their
// order and values.
func renameKeys(raw json.RawMessage, rename func(string) string) (json.RawMessage, error) {
	dec := json.NewDecoder(bytes.NewReader(raw))
	tok, err := dec.Token()
	if err != nil {
		return nil, err
	}
	if d, ok := tok.(json.Delim); !ok || d != '{' {
		return nil, errors.New("not an object")
	}
	var out bytes.Buffer
	out.WriteByte('{')
	first := true
	for dec.More() {
		keyTok, err := dec.Token()
		if err != nil {
			return nil, err
		}
		key, _ := keyTok.(string)
		var value json.RawMessage
		if err := dec.Decode(&value); err != nil {
			return nil, err
		}
		if !first {
			out.WriteByte(',')
		}
		first = false
		name, err := json.Marshal(rename(key))
		if err != nil {
			return nil, err
		}
		out.Write(name)
		out.WriteByte(':')
		if err := json.Compact(&out, value); err != nil {
			return nil, err
		}
	}
	out.WriteByte('}')
	return out.Bytes(), nil
}
