package records

import (
	"math"
	"strconv"
	"unicode/utf8"
)

// Field is one key and its value as JSON, spelled as it was given: 1.2300, -0,
// a big integer and a nested object come back byte for byte.
type Field struct {
	Key   string
	Value string
}

func String(key, value string) Field {
	return Field{Key: key, Value: string(appendJSONString(nil, value))}
}

func Int(key string, value int64) Field {
	return Field{Key: key, Value: strconv.FormatInt(value, 10)}
}

// Float writes the shortest spelling that reads back as the same float; NaN
// and the infinities, which JSON has no number for, become the strings
// "NaN", "+Inf" and "-Inf".
func Float(key string, value float64) Field {
	return Field{Key: key, Value: string(appendJSONFloat(nil, value))}
}

func Bool(key string, value bool) Field {
	return Field{Key: key, Value: strconv.FormatBool(value)}
}

// JSON keeps value as it is; Append refuses it unless it is valid JSON.
func JSON(key string, value []byte) Field {
	return Field{Key: key, Value: string(value)}
}

func appendJSONFloat(out []byte, value float64) []byte {
	if math.IsNaN(value) || math.IsInf(value, 0) {
		return appendJSONString(out, strconv.FormatFloat(value, 'g', -1, 64))
	}
	return strconv.AppendFloat(out, value, 'g', -1, 64)
}

// appendJSONString quotes as encoding/json does, without escaping HTML: control
// characters and the line and paragraph separators are escaped, and an invalid
// byte becomes the replacement character
func appendJSONString(out []byte, value string) []byte {
	const hex = "0123456789abcdef"
	out = append(out, '"')
	for i := 0; i < len(value); {
		r, size := utf8.DecodeRuneInString(value[i:])
		switch {
		case r == '"' || r == '\\':
			out = append(out, '\\', byte(r))
		case r == '\n':
			out = append(out, '\\', 'n')
		case r == '\r':
			out = append(out, '\\', 'r')
		case r == '\t':
			out = append(out, '\\', 't')
		case r == '\b':
			out = append(out, '\\', 'b')
		case r == '\f':
			out = append(out, '\\', 'f')
		case r == utf8.RuneError && size == 1:
			out = utf8.AppendRune(out, utf8.RuneError)
		case r < 0x20 || r == 0x2028 || r == 0x2029:
			out = append(out, '\\', 'u', hex[r>>12&0xf], hex[r>>8&0xf], hex[r>>4&0xf], hex[r&0xf])
		default:
			out = append(out, value[i:i+size]...)
		}
		i += size
	}
	return append(out, '"')
}
