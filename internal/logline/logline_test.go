package logline

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"flag"
	"log/slog"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"
)

var update = flag.Bool("update", false, "write testdata/console.json's lines from what Go writes")

// consoleVectors is testdata/console.json, which the Bun and Python loggers'
// tests read too
type consoleVectors struct {
	About   string          `json:"about"`
	Secrets []string        `json:"secrets"`
	Lines   []consoleVector `json:"lines"`
}

type consoleVector struct {
	Name             string               `json:"name"`
	At               string               `json:"at"`
	Stream           string               `json:"stream"`
	Level            *int                 `json:"level,omitempty"`
	Event            string               `json:"event,omitempty"`
	Msg              *string              `json:"msg,omitempty"`
	Context          [][2]json.RawMessage `json:"context,omitempty"`
	Attrs            [][2]json.RawMessage `json:"attrs,omitempty"`
	TraceID          string               `json:"trace_id,omitempty"`
	SpanID           string               `json:"span_id,omitempty"`
	Redact           []string             `json:"redact,omitempty"`
	RedactSecrets    bool                 `json:"redact_secrets,omitempty"`
	KeepURLPasswords bool                 `json:"keep_url_passwords,omitempty"`
	Color            bool                 `json:"color,omitempty"`
	Time             string               `json:"time,omitempty"`
	HideStream       bool                 `json:"hide_stream,omitempty"`
	JSON             string               `json:"json"`
	Pretty           string               `json:"pretty"`
}

func TestConsoleLinesAreTheVectors(t *testing.T) {
	path := filepath.Join("..", "..", "testdata", "console", "console.json")
	vectors := readConsoleVectors(t, path)
	if !slices.Equal(vectors.Secrets, Secrets) {
		t.Errorf("the vectors' secrets are %q, Secrets %q", vectors.Secrets, Secrets)
	}
	for i := range vectors.Lines {
		v := &vectors.Lines[i]
		line := v.line(t)
		names := v.Redact
		if v.RedactSecrets {
			names = append(slices.Clone(names), Secrets...)
		}
		redact := newRedactor(names, v.KeepURLPasswords)
		line.Context, line.Attrs = redact.fields(line.Context), redact.fields(line.Attrs)
		gotJSON := string(AppendJSON(nil, &line))
		gotPretty := string(AppendPretty(nil, &line, v.look(t)))
		if *update {
			v.JSON, v.Pretty = gotJSON, gotPretty
			continue
		}
		if gotJSON != v.JSON {
			t.Errorf("%s: JSON\n got %q\nwant %q", v.Name, gotJSON, v.JSON)
		}
		if gotPretty != v.Pretty {
			t.Errorf("%s: pretty\n got %q\nwant %q", v.Name, gotPretty, v.Pretty)
		}
	}
	if *update {
		writeConsoleVectors(t, path, vectors)
	}
}

func readConsoleVectors(t *testing.T, path string) consoleVectors {
	t.Helper()
	text, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var vectors consoleVectors
	if err = json.Unmarshal(text, &vectors); err != nil {
		t.Fatal(err)
	}
	return vectors
}

// writeConsoleVectors writes a vector a line, as server/wire's vectors are
func writeConsoleVectors(t *testing.T, path string, vectors consoleVectors) {
	t.Helper()
	var out bytes.Buffer
	about, _ := json.Marshal(vectors.About)
	secrets, _ := json.Marshal(vectors.Secrets)
	out.WriteString("{\n  \"about\": " + string(about) + ",\n  \"secrets\": " + string(secrets) + ",\n  \"lines\": [\n")
	for i, v := range vectors.Lines {
		var line bytes.Buffer
		encoder := json.NewEncoder(&line)
		encoder.SetEscapeHTML(false)
		if err := encoder.Encode(v); err != nil {
			t.Fatal(err)
		}
		out.WriteString("    " + string(bytes.TrimSpace(line.Bytes())))
		if i < len(vectors.Lines)-1 {
			out.WriteByte(',')
		}
		out.WriteByte('\n')
	}
	out.WriteString("  ]\n}\n")
	if err := os.WriteFile(path, out.Bytes(), 0o600); err != nil {
		t.Fatal(err)
	}
}

// look is how the vector's pretty line shows, in UTC
func (v *consoleVector) look(t *testing.T) Look {
	t.Helper()
	shown, ok := ReadTime(v.Time)
	if v.Time != "" && !ok {
		t.Fatalf("%s: a time of %q", v.Name, v.Time)
	}
	return Look{Color: v.Color, Zone: time.UTC, Time: shown, HideStream: v.HideStream}
}

// line is the vector as the handler would have built it
func (v *consoleVector) line(t *testing.T) Line {
	t.Helper()
	nanos, err := strconv.ParseInt(v.At, 10, 64)
	if err != nil {
		t.Fatal(err)
	}
	line := Line{At: time.Unix(0, nanos), Stream: v.Stream, Name: LogName, Body: v.Msg}
	if v.Event != "" {
		line.Name = v.Event
	}
	if v.Level != nil {
		level := slog.Level(*v.Level)
		line.Level = &level
	}
	line.Context, line.Attrs = vectorFields(t, v.Context), vectorFields(t, v.Attrs)
	copy(line.TraceID[:], decodeHex(t, v.TraceID))
	copy(line.SpanID[:], decodeHex(t, v.SpanID))
	return line
}

func vectorFields(t *testing.T, pairs [][2]json.RawMessage) []Field {
	t.Helper()
	var fields []Field
	for _, pair := range pairs {
		var key string
		if err := json.Unmarshal(pair[0], &key); err != nil {
			t.Fatal(err)
		}
		var value bytes.Buffer
		if err := json.Compact(&value, pair[1]); err != nil {
			t.Fatal(err)
		}
		fields = append(fields, Field{Key: key, Value: value.String()})
	}
	return fields
}

func decodeHex(t *testing.T, text string) []byte {
	t.Helper()
	decoded, err := hex.DecodeString(text)
	if err != nil {
		t.Fatal(err)
	}
	return decoded
}

func TestWordsAreAsAPersonReadsThem(t *testing.T) {
	for key, want := range map[string]string{
		"DB_PASSWORD":       "db password",
		"PasswordHash":      "password hash",
		"APIKey":            "api key",
		"apiKey":            "api key",
		"APIKEY":            "apikey",
		"signing-key":       "signing key",
		"db.password":       "db password",
		"oauth2Token":       "oauth2 token",
		"connection string": "connection string",
		"":                  "",
	} {
		if got := strings.Join(words(key), " "); got != want {
			t.Errorf("%q has the words %q, want %q", key, got, want)
		}
	}
}

// Dashbin's list: every spelling of a secret is hidden by Secrets
func TestRedactHidesASecretWhateverItsKeySpelling(t *testing.T) {
	r := newRedactor(Secrets, false)
	for _, key := range []string{
		"password", "DB_PASSWORD", "passwd", "secret", "bot_token", "credential", "Authorization",
		"api_key", "apikey", "private_key", "dsn", "accessKey", "connection_string", "PasswordHash", "signing-key",
		"x-api-key", "client_secret", "Set-Cookie", "db.password",
	} {
		if !r.hides(key) {
			t.Errorf("%s is shown", key)
		}
	}
}

func TestACounterNamedLikeASecretStaysVisible(t *testing.T) {
	r := newRedactor(Secrets, false)
	for _, key := range []string{"tokens_used", "passwords", "secretary", "keyboard", "author", "user"} {
		if r.hides(key) {
			t.Errorf("%s is hidden", key)
		}
	}
	if !newRedactor([]string{"token"}, false).hides("token_count") {
		t.Error("token hides token_count, a word of its own")
	}
}

func TestAPasswordInsideAURLIsHidden(t *testing.T) {
	for text, want := range map[string]string{
		`"postgres://dashbin:hunter2@db.internal:5432/app"`:                 `"postgres://dashbin:[redacted]@db.internal:5432/app"`,
		`"see https://ann:p%40ss@example.com/x and redis://:pw@cache:6379"`: `"see https://ann:[redacted]@example.com/x and redis://:[redacted]@cache:6379"`,
		`{"source":"mysql://u:p@h/db","n":1}`:                               `{"source":"mysql://u:[redacted]@h/db","n":1}`,
		`"https://ann@example.com/x"`:                                       `"https://ann@example.com/x"`,
		`"https://example.com:8443/x?user=a:b@c"`:                           `"https://example.com:8443/x?user=a:b@c"`,
		`"user:pass@host, no scheme"`:                                       `"user:pass@host, no scheme"`,
		`"ftp://u:@host"`:                                                   `"ftp://u:@host"`,
		`"://u:p@host"`:                                                     `"://u:p@host"`,
	} {
		if got := hideURLPasswords(text); got != want {
			t.Errorf("%s\n got %s\nwant %s", text, got, want)
		}
	}
}

// a logger hides a URL's password unless told to keep it, in the store and on the console alike
func TestURLPasswordsAreHiddenThroughJSONEscapes(t *testing.T) {
	quote, slash := string(rune(34)), string(rune(92))
	uri := "postgres://ann:hunter2@db/app"
	escaped := strings.ReplaceAll(uri, "/", slash+"/")
	unicodeEscaped := strings.NewReplacer(":", slash+"u003a", "/", slash+"u002f", "@", slash+"u0040").Replace(uri)
	for _, text := range []string{escaped, unicodeEscaped} {
		spelled := quote + text + quote
		got := hideURLPasswords(spelled)
		var decoded string
		if err := json.Unmarshal([]byte(got), &decoded); err != nil || decoded != "postgres://ann:[redacted]@db/app" {
			t.Fatalf("an escaped URL: %s: %v", got, err)
		}
		var out bytes.Buffer
		slog.New(NewHandler("api", nil, Settings{Format: JSON, To: &out}, nil)).Info("connect", "source", json.RawMessage(spelled))
		if strings.Contains(out.String(), "hunter2") {
			t.Fatalf("the console exposed the password: %s", out.String())
		}
	}
	spelled := "{" + quote + "source" + quote + ":" + quote + escaped + quote + "," + quote + "n" + quote + ":9007199254740993}"
	got := hideURLPasswords(spelled)
	if strings.Contains(got, "hunter2") || !strings.Contains(got, "9007199254740993") {
		t.Fatalf("a nested URL changed an exact number: %s", got)
	}
}
