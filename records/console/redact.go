package console

import (
	"bytes"
	"encoding/json"
	"fmt"
	"strings"
	"unicode"

	"github.com/tinyshed/tinystore/records/internal/logline"
)

// redacted is what a hidden value becomes, in the record and on the console
const redacted = `"[redacted]"`

// redactor hides the values of the fields a handler was told to, at any depth
// of a JSON object, and the password of a URL inside a value. The message is
// not searched.
//
//	Redact("password"):  user={"name":"ann","password":"x"}  →  user={"name":"ann","password":"[redacted]"}
type redactor struct {
	names []string // each name's words written together: "api key" is "apikey"
	urls  bool     // hide a URL's password
}

func newRedactor(names []string, keepURLPasswords bool) redactor {
	r := redactor{urls: !keepURLPasswords}
	for _, name := range names {
		if joined := strings.Join(words(name), ""); joined != "" {
			r.names = append(r.names, joined)
		}
	}
	return r
}

func (r redactor) none() bool {
	return len(r.names) == 0 && !r.urls
}

// words splits a key as a person reads it, in lower case: at anything but a
// letter or a digit, and where a capital begins a word
//
//	DB_PASSWORD → db password    PasswordHash → password hash    APIKey → api key    signing-key → signing key
func words(key string) []string {
	var out []string
	runes := []rune(key)
	start := -1
	for i, r := range runes {
		if !unicode.IsLetter(r) && !unicode.IsDigit(r) {
			if start >= 0 {
				out = append(out, strings.ToLower(string(runes[start:i])))
				start = -1
			}
			continue
		}
		if start >= 0 && startsWord(runes, i) {
			out = append(out, strings.ToLower(string(runes[start:i])))
			start = i
		}
		if start < 0 {
			start = i
		}
	}
	if start >= 0 {
		out = append(out, strings.ToLower(string(runes[start:])))
	}
	return out
}

// startsWord is a capital after a small letter or a digit, or a capital
// before a small letter after capitals: passwordHash, APIKey
func startsWord(runes []rune, i int) bool {
	if !unicode.IsUpper(runes[i]) {
		return false
	}
	before := runes[i-1]
	lowerAfter := i+1 < len(runes) && unicode.IsLower(runes[i+1])
	return unicode.IsLower(before) || unicode.IsDigit(before) || unicode.IsUpper(before) && lowerAfter
}

// hides says whether a key names a secret: some of its words in a row,
// written together, are a name's
func (r redactor) hides(key string) bool {
	if len(r.names) == 0 {
		return false
	}
	keyWords := words(key)
	for _, name := range r.names {
		if joinsTo(keyWords, name) {
			return true
		}
	}
	return false
}

// joinsTo says whether some words in a row, written together, are name
func joinsTo(words []string, name string) bool {
	for i := range words {
		rest := name
		for _, word := range words[i:] {
			if !strings.HasPrefix(rest, word) {
				break
			}
			rest = rest[len(word):]
			if rest == "" {
				return true
			}
		}
	}
	return false
}

// fields hides what it must, copying the fields only when one changes, since
// a handler's context is shared by every line it logs
func (r redactor) fields(fields []logline.Field) []logline.Field {
	if r.none() {
		return fields
	}
	var hidden []logline.Field
	for i, field := range fields {
		value := field.Value
		if r.hides(field.Key) {
			value = redacted
		} else {
			value = r.value(value)
		}
		if value == field.Value {
			continue
		}
		if hidden == nil {
			hidden = append([]logline.Field(nil), fields...)
		}
		hidden[i].Value = value
	}
	if hidden == nil {
		return fields
	}
	return hidden
}

// value is a field's JSON with the secrets inside it hidden
func (r redactor) value(spelled string) string {
	spelled = r.json(spelled)
	if r.urls {
		spelled = hideURLPasswords(spelled)
	}
	return spelled
}

// json hides the values of an object's keys at any depth, keeping the order
// of the keys and the spelling of everything else; text that is no object or
// array, and text whose keys name no secret, comes back as it was
func (r redactor) json(spelled string) string {
	if len(r.names) == 0 || spelled == "" || spelled[0] != '{' && spelled[0] != '[' {
		return spelled
	}
	var out bytes.Buffer
	changed, err := r.rewrite(&out, json.RawMessage(spelled))
	if err != nil || !changed {
		return spelled
	}
	return out.String()
}

// rewrite writes raw with the values of secret keys hidden, and says whether
// it hid any
func (r redactor) rewrite(out *bytes.Buffer, raw json.RawMessage) (bool, error) {
	raw = bytes.TrimSpace(raw)
	if len(raw) == 0 || raw[0] != '{' && raw[0] != '[' {
		out.Write(raw)
		return false, nil
	}
	decoder := json.NewDecoder(bytes.NewReader(raw))
	if _, err := decoder.Token(); err != nil {
		return false, err
	}
	out.WriteByte(raw[0])
	changed := false
	for first := true; decoder.More(); first = false {
		if !first {
			out.WriteByte(',')
		}
		key, hide, err := r.key(decoder, raw[0] == '{')
		if err != nil {
			return false, err
		}
		out.Write(key)
		var value json.RawMessage
		if err = decoder.Decode(&value); err != nil {
			return false, err
		}
		if hide {
			out.WriteString(redacted)
			changed = true
			continue
		}
		inner, err := r.rewrite(out, value)
		if err != nil {
			return false, err
		}
		changed = changed || inner
	}
	if raw[0] == '{' {
		out.WriteByte('}')
	} else {
		out.WriteByte(']')
	}
	return changed, nil
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
	return append(logline.AppendString(nil, key), ':'), r.hides(key), nil
}

// hideURLPasswords hides the password of each URL in JSON text, keeping
// everything else as it was spelled:
//
//	"see postgres://ann:hunter2@db:5432/app"  →  "see postgres://ann:[redacted]@db:5432/app"
func hideURLPasswords(text string) string {
	var out strings.Builder
	written := 0
	for from := 0; from < len(text); {
		found := strings.Index(text[from:], "://")
		if found < 0 {
			break
		}
		schemeEnd := from + found
		password, at, end := findPassword(text, schemeEnd+len("://"))
		from = end
		if password < 0 || !endsInScheme(text[:schemeEnd]) {
			continue
		}
		out.WriteString(text[written:password])
		out.WriteString("[redacted]")
		written = at
	}
	if written == 0 {
		return text
	}
	out.WriteString(text[written:])
	return out.String()
}

// findPassword reads the authority from start: where its password begins and
// the '@' after it, -1 when it has none, and where the authority ends
func findPassword(text string, start int) (password, at, end int) {
	at, colon := -1, -1
	i := start
	for i < len(text) {
		c := text[i]
		if c == '\\' { // an escape of the JSON string, kept whole
			i += 2
			continue
		}
		if c == '/' || c == '?' || c == '#' || c == '"' || c <= ' ' {
			break
		}
		if c == '@' {
			at = i
		} else if c == ':' && colon < 0 {
			colon = i
		}
		i++
	}
	end = min(i, len(text))
	if at < 0 || colon < 0 || colon+1 >= at {
		return -1, -1, end
	}
	return colon + 1, at, end
}

// endsInScheme says whether text ends in a URL's scheme: letters, digits,
// '+', '-' or '.', a letter first
func endsInScheme(text string) bool {
	i := len(text)
	for i > 0 && schemeByte(text[i-1]) {
		i--
	}
	return i < len(text) && letter(text[i])
}

func schemeByte(c byte) bool {
	return letter(c) || '0' <= c && c <= '9' || c == '+' || c == '-' || c == '.'
}

func letter(c byte) bool {
	return 'a' <= c && c <= 'z' || 'A' <= c && c <= 'Z'
}
