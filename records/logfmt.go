package records

import (
	"encoding/json"
	"strings"
)

// logfmtFields is a logfmt line's pairs when spellLogfmt spells them back as
// the line, byte for byte; a line of fewer than two pairs is not taken for one.
// A bare value is kept as the JSON number, true, false or null it spells, or
// else as a JSON string, and a quoted one as the JSON string it is:
//
//	level=info msg="slow request" ms=1200   → level "info", msg "slow request", ms 1200
//	level=info  msg=ok                      → stays text: two spaces between pairs
//	level=info msg="ok"                     → stays text: ok is spelled bare
func logfmtFields(text string) ([]Field, bool) {
	var fields []Field
	for rest := text; rest != ""; {
		key, value, next, ok := nextPair(rest)
		if !ok || len(fields) == maxFields {
			return nil, false
		}
		fields = append(fields, Field{Key: key, Value: value})
		if rest, ok = strings.CutPrefix(next, " "); !ok && next != "" {
			return nil, false
		}
	}
	if len(fields) < 2 || spellLogfmt(fields) != text {
		return nil, false
	}
	return fields, true
}

// nextPair reads the key=value text begins with: a bare value runs to the next
// space, a quoted one to its closing quote
func nextPair(text string) (key, value, rest string, ok bool) {
	equals := strings.IndexByte(text, '=')
	if equals < 1 || !isBare(text[:equals]) {
		return "", "", "", false
	}
	key, text = text[:equals], text[equals+1:]
	if strings.HasPrefix(text, `"`) {
		end := closingQuote(text)
		if end < 0 || !json.Valid([]byte(text[:end+1])) {
			return "", "", "", false
		}
		return key, text[:end+1], text[end+1:], true
	}
	end := strings.IndexByte(text, ' ')
	if end < 0 {
		end = len(text)
	}
	if !isBare(text[:end]) {
		return "", "", "", false
	}
	return key, bareValue(text[:end]), text[end:], true
}

// isBare is text logfmt writes without quotes: no space, quote or control byte
func isBare(text string) bool {
	for i := range len(text) {
		if text[i] <= ' ' || text[i] == '"' || text[i] == 0x7f {
			return false
		}
	}
	return true
}

// closingQuote is where the quoted text begins with ends, past its escapes
func closingQuote(text string) int {
	for i := 1; i < len(text); i++ {
		switch text[i] {
		case '\\':
			i++
		case '"':
			return i
		}
	}
	return -1
}

func bareValue(bare string) string {
	if bare == "true" || bare == "false" || bare == "null" {
		return bare
	}
	if bare != "" && (bare[0] == '-' || isDigit(bare[0])) && json.Valid([]byte(bare)) {
		return bare
	}
	return string(appendJSONString(nil, bare))
}

// spellLogfmt writes fields as a logfmt line. A JSON string is bare when what
// it holds is bare text and quoted as it is otherwise; every other value is
// written as it is.
func spellLogfmt(fields []Field) string {
	var line strings.Builder
	for i, field := range fields {
		if i > 0 {
			line.WriteByte(' ')
		}
		line.WriteString(field.Key)
		line.WriteByte('=')
		line.WriteString(logfmtValue(field.Value))
	}
	return line.String()
}

func logfmtValue(value string) string {
	held, quoted := unquote(value)
	if !quoted {
		return value
	}
	if strings.IndexByte(held, '\\') >= 0 && json.Unmarshal([]byte(value), &held) != nil {
		return value
	}
	if isBare(held) {
		return held
	}
	return value
}
