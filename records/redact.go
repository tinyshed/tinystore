package records

import (
	"bytes"
	"encoding/json"
	"fmt"
	"strings"
)

// redacted is what a hidden value becomes, in the record and on the console
const redacted = `"[redacted]"`

// redactor hides the values of the fields a Handler was told to: a field
// whose key, or the part of a dotted key after its last dot, is one of its
// names, the case ignored, and a key of the same name at any depth of a JSON
// object. The message is not searched.
//
//	Redact("password"):  user={"name":"ann","password":"x"}  →  user={"name":"ann","password":"[redacted]"}
type redactor map[string]struct{}

func newRedactor(names []string) redactor {
	if len(names) == 0 {
		return nil
	}
	r := make(redactor, len(names))
	for _, name := range names {
		r[strings.ToLower(name)] = struct{}{}
	}
	return r
}

func (r redactor) hides(key string) bool {
	key = strings.ToLower(key)
	if _, ok := r[key]; ok {
		return true
	}
	if dot := strings.LastIndexByte(key, '.'); dot >= 0 {
		_, ok := r[key[dot+1:]]
		return ok
	}
	return false
}

// fields hides what it must, copying the fields only when one changes, since
// a handler's context is shared by every line it logs
func (r redactor) fields(fields []Field) []Field {
	if len(r) == 0 {
		return fields
	}
	var hidden []Field
	for i, field := range fields {
		value := field.Value
		if r.hides(field.Key) {
			value = redacted
		} else {
			value = r.json(value)
		}
		if value == field.Value {
			continue
		}
		if hidden == nil {
			hidden = append([]Field(nil), fields...)
		}
		hidden[i].Value = value
	}
	if hidden == nil {
		return fields
	}
	return hidden
}

// json hides the values of an object's keys at any depth, keeping the order
// of the keys and the spelling of everything else; text no name appears in,
// and text that is no object or array, comes back as it was
func (r redactor) json(spelled string) string {
	if spelled == "" || spelled[0] != '{' && spelled[0] != '[' || !r.mentioned(spelled) {
		return spelled
	}
	var out bytes.Buffer
	if r.rewrite(&out, json.RawMessage(spelled)) != nil {
		return spelled
	}
	return out.String()
}

// mentioned says whether any name appears in the text at all, so that a value
// needing no change is not decoded
func (r redactor) mentioned(text string) bool {
	text = strings.ToLower(text)
	for name := range r {
		if strings.Contains(text, name) {
			return true
		}
	}
	return false
}

func (r redactor) rewrite(out *bytes.Buffer, raw json.RawMessage) error {
	raw = bytes.TrimSpace(raw)
	if len(raw) == 0 || raw[0] != '{' && raw[0] != '[' {
		out.Write(raw)
		return nil
	}
	decoder := json.NewDecoder(bytes.NewReader(raw))
	if _, err := decoder.Token(); err != nil {
		return err
	}
	out.WriteByte(raw[0])
	for first := true; decoder.More(); first = false {
		if !first {
			out.WriteByte(',')
		}
		key, hide, err := r.key(decoder, raw[0] == '{')
		if err != nil {
			return err
		}
		out.Write(key)
		var value json.RawMessage
		if err := decoder.Decode(&value); err != nil {
			return err
		}
		if hide {
			out.WriteString(redacted)
		} else if err := r.rewrite(out, value); err != nil {
			return err
		}
	}
	if raw[0] == '{' {
		out.WriteByte('}')
	} else {
		out.WriteByte(']')
	}
	return nil
}

// key reads an object's next key, spelled again with its colon, and whether
// its value is hidden; an array's element has none
func (r redactor) key(decoder *json.Decoder, object bool) ([]byte, bool, error) {
	if !object {
		return nil, false, nil
	}
	token, err := decoder.Token()
	if err != nil {
		return nil, false, err
	}
	key, ok := token.(string)
	if !ok {
		return nil, false, fmt.Errorf("an object's key is %v", token)
	}
	return append(appendJSONString(nil, key), ':'), r.hides(key), nil
}
