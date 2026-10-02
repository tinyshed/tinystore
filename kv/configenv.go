package kv

import (
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"reflect"
	"strconv"
	"strings"
	"time"
	"unicode"

	"github.com/tinyshed/tinystore"
)

// envName is a field's variable: the prefix and its path in upper snake case.
// The Bun and Python SDKs name it the same way (testdata/config.json).
//
//	"APP", limits.maxRps → APP_LIMITS_MAX_RPS
//	"",    dbUrl         → DB_URL
func envName(prefix, path string) string {
	name := upperSnake(path)
	if prefix == "" {
		return name
	}
	return prefix + "_" + name
}

// upperSnake spells a path's words apart in capitals: a dot, a dash, or a
// capital after a small letter or a digit, or one before a small letter after
// capitals, begins a word
//
//	dbUrl → DB_URL, HTTPServer → HTTP_SERVER, v2Api → V2_API
func upperSnake(path string) string {
	var out strings.Builder
	runes := []rune(path)
	for i, r := range runes {
		if r == '.' || r == '-' {
			out.WriteByte('_')
			continue
		}
		if i > 0 && unicode.IsUpper(r) {
			before := runes[i-1]
			lowerAfter := i+1 < len(runes) && unicode.IsLower(runes[i+1])
			if unicode.IsLower(before) || unicode.IsDigit(before) || unicode.IsUpper(before) && lowerAfter {
				out.WriteByte('_')
			}
		}
		out.WriteRune(unicode.ToUpper(r))
	}
	return out.String()
}

// readEnvironment is the variables of files, a later file's over an earlier's,
// and the process's over them all, as dotenv libraries take them. A file that
// is not there is skipped: production has none.
func readEnvironment(files []string) (func(string) (string, bool), error) {
	fromFiles := map[string]string{}
	for _, file := range files {
		text, err := os.ReadFile(file) //nolint:gosec // a file the program names
		if errors.Is(err, fs.ErrNotExist) {
			continue
		}
		if err != nil {
			return nil, err
		}
		read, err := parseDotenv(string(text))
		if err != nil {
			return nil, fmt.Errorf("%w: %s: %w", tinystore.ErrInvalid, file, err)
		}
		for name, value := range read {
			fromFiles[name] = value
		}
	}
	return func(name string) (string, bool) {
		if value, ok := os.LookupEnv(name); ok {
			return value, true
		}
		value, ok := fromFiles[name]
		return value, ok
	}, nil
}

// parseDotenv reads the lines of a .env file
//
//	# a comment
//	export PORT=3000
//	URL=postgres://localhost/app  # a comment after a space
//	GREETING="two\nlines"         # \n, \r, \t, \" and \\ inside double quotes
//	PATTERN='$literal\n'          # nothing is read inside single quotes
//
// A quoted value may run over lines; a later name wins.
func parseDotenv(text string) (map[string]string, error) {
	values := map[string]string{}
	for line := 1; text != ""; {
		name, value, rest, err := dotenvAssignment(text)
		if err != nil {
			return nil, fmt.Errorf("line %d: %w", line, err)
		}
		line += strings.Count(text[:len(text)-len(rest)], "\n")
		if name != "" {
			values[name] = value
		}
		text = rest
	}
	return values, nil
}

// dotenvAssignment reads one line, or the lines of a quoted value, and returns
// the name and value it assigns, none for a blank or a comment, and the text
// after it
func dotenvAssignment(text string) (name, value, rest string, err error) {
	line, rest, _ := strings.Cut(text, "\n")
	trimmed := strings.TrimSpace(line)
	if trimmed == "" || trimmed[0] == '#' {
		return "", "", rest, nil
	}
	equals := strings.IndexByte(line, '=')
	if equals < 0 {
		return "", "", "", fmt.Errorf("%q is no NAME=value", line)
	}
	name = strings.TrimSpace(strings.TrimPrefix(strings.TrimSpace(line[:equals]), "export "))
	if !validEnvName(name) {
		return "", "", "", fmt.Errorf("%q is no variable's name", name)
	}
	start := equals + 1
	for start < len(line) && (line[start] == ' ' || line[start] == '\t') {
		start++
	}
	if start < len(line) && (line[start] == '"' || line[start] == '\'') {
		value, rest, err = dotenvQuoted(text[start:])
		return name, value, rest, err
	}
	value = line[start:]
	if comment := strings.Index(value, " #"); comment >= 0 {
		value = value[:comment]
	}
	return name, strings.TrimRight(value, " \t\r"), rest, nil
}

// dotenvQuoted reads a quoted value from its opening quote, and returns it
// and the text after the line its closing quote is on
func dotenvQuoted(text string) (value, rest string, err error) {
	quote := text[0]
	var out strings.Builder
	for i := 1; i < len(text); i++ {
		c := text[i]
		switch {
		case c == quote:
			_, rest, _ = strings.Cut(text[i+1:], "\n")
			return out.String(), rest, nil
		case c == '\\' && quote == '"' && i+1 < len(text):
			i++
			out.WriteByte(unescaped(text[i]))
		default:
			out.WriteByte(c)
		}
	}
	return "", "", fmt.Errorf("a value opened with %c is never closed", quote)
}

func unescaped(c byte) byte {
	switch c {
	case 'n':
		return '\n'
	case 'r':
		return '\r'
	case 't':
		return '\t'
	}
	return c
}

// validEnvName is a variable's name: letters, digits after the first, and _
func validEnvName(name string) bool {
	for i, r := range name {
		if r != '_' && !unicode.IsLetter(r) && (i == 0 || !unicode.IsDigit(r)) {
			return false
		}
	}
	return name != ""
}

var durationType = reflect.TypeFor[time.Duration]()

// envJSON is a variable's text as the JSON of a field of type t: a number, a
// bool or a duration read as Go reads one, a list split at its commas or given
// as JSON, and anything else as JSON
//
//	int "3000" → 3000, []string "a.com, b.com" → ["a.com","b.com"], time.Duration "1h30m" → 5400000000000
func envJSON(text string, t reflect.Type) (json.RawMessage, error) {
	trimmed := strings.TrimSpace(text)
	if t.Kind() == reflect.Slice && t.Elem().Kind() != reflect.Uint8 && !strings.HasPrefix(trimmed, "[") {
		return envList(trimmed, t.Elem())
	}
	return envScalar(text, t)
}

func envList(text string, elem reflect.Type) (json.RawMessage, error) {
	items := []json.RawMessage{}
	if text != "" {
		for part := range strings.SplitSeq(text, ",") {
			item, err := envScalar(strings.TrimSpace(part), elem)
			if err != nil {
				return nil, err
			}
			items = append(items, item)
		}
	}
	return json.Marshal(items)
}

func envScalar(text string, t reflect.Type) (json.RawMessage, error) {
	trimmed := strings.TrimSpace(text)
	switch {
	case t == durationType:
		d, err := time.ParseDuration(trimmed)
		if err != nil {
			return nil, fmt.Errorf("%q is no duration", text)
		}
		return json.Marshal(d)
	case t.Kind() == reflect.String:
		return json.Marshal(text)
	case t.Kind() == reflect.Bool:
		b, err := strconv.ParseBool(trimmed)
		if err != nil {
			return nil, fmt.Errorf("%q is not true or false", text)
		}
		return json.Marshal(b)
	case t.Kind() >= reflect.Int && t.Kind() <= reflect.Int64:
		n, err := strconv.ParseInt(trimmed, 10, t.Bits())
		if err != nil {
			return nil, fmt.Errorf("%q is no %s", text, t)
		}
		return json.Marshal(n)
	case t.Kind() >= reflect.Uint && t.Kind() <= reflect.Uint64:
		n, err := strconv.ParseUint(trimmed, 10, t.Bits())
		if err != nil {
			return nil, fmt.Errorf("%q is no %s", text, t)
		}
		return json.Marshal(n)
	case t.Kind() == reflect.Float32 || t.Kind() == reflect.Float64:
		f, err := strconv.ParseFloat(trimmed, t.Bits())
		if err != nil {
			return nil, fmt.Errorf("%q is no number", text)
		}
		return json.Marshal(f)
	}
	if !json.Valid([]byte(trimmed)) {
		return nil, fmt.Errorf("%q is no JSON for a %s", text, t)
	}
	return json.RawMessage(trimmed), nil
}
