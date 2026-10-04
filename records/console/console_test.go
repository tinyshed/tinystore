package console

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"flag"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/records/internal/logline"
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
	path := filepath.Join("testdata", "console.json")
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
		gotJSON := string(logline.AppendJSON(nil, &line))
		gotPretty := string(logline.AppendPretty(nil, &line, v.look(t)))
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
func (v *consoleVector) look(t *testing.T) logline.Look {
	t.Helper()
	shown, ok := readTime(v.Time)
	if v.Time != "" && !ok {
		t.Fatalf("%s: a time of %q", v.Name, v.Time)
	}
	return logline.Look{Color: v.Color, Zone: time.UTC, Time: lineTime(shown), HideStream: v.HideStream}
}

// line is the vector as the handler would have built it
func (v *consoleVector) line(t *testing.T) logline.Line {
	t.Helper()
	nanos, err := strconv.ParseInt(v.At, 10, 64)
	if err != nil {
		t.Fatal(err)
	}
	line := logline.Line{At: time.Unix(0, nanos), Stream: v.Stream, Name: logline.LogName, Body: v.Msg}
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

func vectorFields(t *testing.T, pairs [][2]json.RawMessage) []logline.Field {
	t.Helper()
	var fields []logline.Field
	for _, pair := range pairs {
		var key string
		if err := json.Unmarshal(pair[0], &key); err != nil {
			t.Fatal(err)
		}
		var value bytes.Buffer
		if err := json.Compact(&value, pair[1]); err != nil {
			t.Fatal(err)
		}
		fields = append(fields, logline.Field{Key: key, Value: value.String()})
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

// writeTo sends a handler's lines to a test's writer, in UTC
type writeTo struct{ io.Writer }

func (w writeTo) apply(s *settings) { s.to, s.zone = w.Writer, time.UTC }

func TestToWritesTheConsoleWhereItIsTold(t *testing.T) {
	t.Setenv("FORCE_COLOR", "")
	var buffer, pretty bytes.Buffer
	slog.New(Handler("api", To(&buffer))).Info("started")
	if !strings.HasPrefix(buffer.String(), `{"time":`) {
		t.Fatalf("a buffer, which is no terminal, has %q", buffer.String())
	}
	slog.New(Handler("api", To(&pretty), Pretty, TimeOff, HideStream)).Info("started", "port", 3000)
	if want := "INFO  started  port=3000\n"; pretty.String() != want {
		t.Fatalf("pretty %q, want %q", pretty.String(), want)
	}
}

// a line reaches its console as it is logged: pretty with a duration as a
// person reads it, JSON with the nanoseconds a record keeps
func TestEachLineReachesTheConsoleAsItIsLogged(t *testing.T) {
	var pretty, lines bytes.Buffer
	line := slog.NewRecord(time.Date(2026, 10, 2, 11, 2, 11, 123_000_000, time.UTC), slog.LevelWarn, "slow request", 0)
	line.AddAttrs(slog.Duration("took", 1500*time.Millisecond))
	with := []slog.Attr{slog.String("requestId", "7f3a")}
	for _, handler := range []slog.Handler{
		Handler("api", Pretty, writeTo{&pretty}).WithAttrs(with),
		Handler("api", JSON, writeTo{&lines}).WithAttrs(with),
	} {
		if err := handler.Handle(t.Context(), line); err != nil {
			t.Fatal(err)
		}
	}
	if want := "11:02:11.123 WARN  api  slow request  requestId=7f3a took=1.5s\n"; pretty.String() != want {
		t.Errorf("pretty %q, want %q", pretty.String(), want)
	}
	want := `{"time":"2026-10-02T11:02:11.123Z","level":"WARN","stream":"api","msg":"slow request",` +
		`"requestId":"7f3a","took":1500000000}` + "\n"
	if lines.String() != want {
		t.Errorf("JSON %q, want %q", lines.String(), want)
	}
}

// lines logged from many goroutines at once each reach the console whole
func TestConsoleLinesFromManyGoroutinesDoNotInterleave(t *testing.T) {
	var console bytes.Buffer
	logger := slog.New(Handler("api", JSON, writeTo{&console})).With("worker", "w")
	var group sync.WaitGroup
	for worker := range 8 {
		group.Go(func() {
			for i := range 100 {
				logger.Info("step", "worker", worker, "i", i, "text", strings.Repeat("x", 200))
			}
		})
	}
	group.Wait()
	lines := strings.Split(strings.TrimSuffix(console.String(), "\n"), "\n")
	if len(lines) != 800 {
		t.Fatalf("%d lines", len(lines))
	}
	for _, line := range lines {
		if !json.Valid([]byte(line)) {
			t.Fatalf("a line is not whole: %q", line)
		}
	}
}

// a handler of the console alone writes its lines as a store's handler writes
// them, and takes none when its console is off
func TestAHandlerWritesTheConsoleAlone(t *testing.T) {
	var console bytes.Buffer
	logger := slog.New(Handler("app", Pretty, writeTo{&console}, Redact("password"))).With("module", "billing")
	logger.Info("charged", "password", "hunter2", "amount", 25)
	if want := " INFO  app  charged  module=billing password=[redacted] amount=25\n"; !strings.HasSuffix(console.String(), want) {
		t.Fatalf("the console has %q, want it to end %q", console.String(), want)
	}
	if Handler("app", Off).Enabled(t.Context(), slog.LevelError) {
		t.Fatal("a handler with neither a store nor a console takes lines")
	}
}

// the records engine's own lines reach the console from Info up, since it
// writes a Debug summary of each flush
func TestTheEnginesOwnLinesReachTheConsoleFromInfoUp(t *testing.T) {
	var console bytes.Buffer
	logger := slog.New(Handler("app", JSON, writeTo{&console}))
	logger.With("engine", "records").Info("flushed")
	logger.Info("sealed", "engine", "records")
	logger.With("engine", "records").Debug("flush finished")
	logger.Debug("flush finished", "engine", "records")
	if lines := strings.Count(console.String(), "\n"); lines != 2 {
		t.Fatalf("the console has %d lines: %s", lines, console.String())
	}
}

// Format and Time are the line's own numbers, which records' Printer converts
func TestAFormatIsTheLinesFormat(t *testing.T) {
	for format, want := range map[Format]logline.Format{0: logline.FormatDefault, Pretty: logline.Pretty, JSON: logline.JSON, Off: logline.Off} {
		if logline.Format(format) != want {
			t.Errorf("console's format %d is the line's %d", format, want)
		}
	}
}
