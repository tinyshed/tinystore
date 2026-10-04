package records

import (
	"strconv"

	"github.com/tinyshed/tinystore/records/internal/logline"
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

// Float writes the shortest spelling that reads back as the same float. NaN and
// the infinities, which JSON has no number for, become the strings "NaN",
// "+Inf" and "-Inf".
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
	return logline.AppendFloat(out, value)
}

func appendJSONString(out []byte, value string) []byte {
	return logline.AppendString(out, value)
}
