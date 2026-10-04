package records

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"log/slog"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"
)

// consoleVectors is testdata/console.json, which the Bun and Python loggers'
// tests read too
type consoleVectors struct {
	About string          `json:"about"`
	Lines []consoleVector `json:"lines"`
}

type consoleVector struct {
	Name       string               `json:"name"`
	At         string               `json:"at"`
	Stream     string               `json:"stream"`
	Level      *int                 `json:"level,omitempty"`
	Event      string               `json:"event,omitempty"`
	Msg        *string              `json:"msg,omitempty"`
	Context    [][2]json.RawMessage `json:"context,omitempty"`
	Attrs      [][2]json.RawMessage `json:"attrs,omitempty"`
	TraceID    string               `json:"trace_id,omitempty"`
	SpanID     string               `json:"span_id,omitempty"`
	Redact     []string             `json:"redact,omitempty"`
	Color      bool                 `json:"color,omitempty"`
	Time       string               `json:"time,omitempty"`
	HideStream bool                 `json:"hide_stream,omitempty"`
	JSON       string               `json:"json"`
	Pretty     string               `json:"pretty"`
}

func TestConsoleLinesAreTheVectors(t *testing.T) {
	path := filepath.Join("testdata", "console.json")
	vectors := readConsoleVectors(t, path)
	for i := range vectors.Lines {
		v := &vectors.Lines[i]
		record := v.record(t)
		redact := newRedactor(v.Redact)
		record.Context, record.Attrs = redact.fields(record.Context), redact.fields(record.Attrs)
		gotJSON := string(appendJSONLine(nil, &record))
		gotPretty := string(appendPretty(nil, &record, v.look(t)))
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
	out.WriteString("{\n  \"about\": " + string(about) + ",\n  \"lines\": [\n")
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
func (v *consoleVector) look(t *testing.T) look {
	t.Helper()
	shown, ok := readConsoleTime(v.Time)
	if v.Time != "" && !ok {
		t.Fatalf("%s: a time of %q", v.Name, v.Time)
	}
	return look{color: v.Color, zone: time.UTC, time: shown, hideStream: v.HideStream}
}

// record is the vector as the handler would have built it
func (v *consoleVector) record(t *testing.T) Record {
	t.Helper()
	nanos, err := strconv.ParseInt(v.At, 10, 64)
	if err != nil {
		t.Fatal(err)
	}
	record := Record{At: time.Unix(0, nanos), Stream: v.Stream, Name: logName, Body: v.Msg}
	if v.Event != "" {
		record.Name = v.Event
	}
	if v.Level != nil {
		level := slog.Level(*v.Level)
		record.Level = &level
	}
	record.Context, record.Attrs = vectorFields(t, v.Context), vectorFields(t, v.Attrs)
	copy(record.TraceID[:], decodeHex(t, v.TraceID))
	copy(record.SpanID[:], decodeHex(t, v.SpanID))
	return record
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

// a Printer writes a record as a Handler's console writes its line: one JSON
// object a line, or the pretty line, and nothing when its console is off
func TestAPrinterWritesARecordAsTheConsoleDoes(t *testing.T) {
	level, body := slog.LevelWarn, "slow request"
	record := Record{
		At: time.Date(2026, 10, 2, 11, 2, 11, 123_000_000, time.UTC), Stream: "api", Name: logName,
		Level: &level, Body: &body, Attrs: []Field{{Key: "ms", Value: "1200"}},
	}
	for _, c := range []struct {
		console Console
		want    []byte
	}{
		{ConsoleJSON, appendJSONLine(nil, &record)},
		{ConsolePretty, appendPretty(nil, &record, look{zone: time.Local})},
		{ConsoleOff, nil},
	} {
		var printed bytes.Buffer
		NewPrinter(&printed, c.console).Print(record)
		if !bytes.Equal(printed.Bytes(), c.want) {
			t.Fatalf("console %d printed %q, want %q", c.console, printed.Bytes(), c.want)
		}
	}
}

func TestToWritesTheConsoleWhereItIsTold(t *testing.T) {
	t.Setenv("FORCE_COLOR", "")
	var buffer, pretty bytes.Buffer
	slog.New(Handler("api", To(&buffer))).Info("started")
	if !strings.HasPrefix(buffer.String(), `{"time":`) {
		t.Fatalf("a buffer, which is no terminal, has %q", buffer.String())
	}
	slog.New(Handler("api", To(&pretty), ConsolePretty, TimeOff, HideStream)).Info("started", "port", 3000)
	if want := "INFO  started  port=3000\n"; pretty.String() != want {
		t.Fatalf("pretty %q, want %q", pretty.String(), want)
	}
}
