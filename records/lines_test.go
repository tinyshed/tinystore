package records

import (
	"fmt"
	"io"
	"log/slog"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// linesOf writes text to a writer of Lines, closes it, and reads back what it wrote
func (s *testStore) linesOf(t *testing.T, stream, text string) []Record {
	t.Helper()
	w := s.Lines(stream)
	if _, err := io.WriteString(w, text); err != nil {
		t.Fatal(err)
	}
	if err := w.Close(); err != nil {
		t.Fatal(err)
	}
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	return s.readAll(t, Query{Streams: []string{stream}})
}

func bodies(records []Record) []string {
	var out []string
	for _, record := range records {
		if record.Body != nil {
			out = append(out, *record.Body)
		}
	}
	return out
}

// the examples in the comment on joins, and a Java exception
func TestLinesJoinWhatBelongsTogether(t *testing.T) {
	s := openRecords(t)
	for _, test := range []struct {
		stream, text string
		want         []string
	}{
		{
			"python", "2026-09-26 12:00:01,500 ERROR request failed\nTraceback (most recent call last):\n" +
				"  File \"app.py\", line 3, in handle\nValueError: bad input\n2026-09-26 12:00:02,000 INFO next\n",
			[]string{"2026-09-26 12:00:01,500 ERROR request failed\nTraceback (most recent call last):\n" +
				"  File \"app.py\", line 3, in handle\nValueError: bad input", "2026-09-26 12:00:02,000 INFO next"},
		},
		{
			"bot", "{\n\"status\": 200\n}\n[2026-09-26 12:00:03] info done\n",
			[]string{"{\n\"status\": 200\n}", "[2026-09-26 12:00:03] info done"},
		},
		{
			"colors", "[2026-09-26 12:00:03] \x1b[32minfo\x1b[39m (app\n[2026-09-26 12:00:04] next\n",
			[]string{"[2026-09-26 12:00:03] \x1b[32minfo\x1b[39m (app", "[2026-09-26 12:00:04] next"},
		},
		{
			"java", "Exception in thread \"main\" java.lang.IllegalStateException: boom\n" +
				"\tat com.example.App.run(App.java:12)\nCaused by: java.io.IOException: disk\n\t... 2 more\nplain\n",
			[]string{"Exception in thread \"main\" java.lang.IllegalStateException: boom\n" +
				"\tat com.example.App.run(App.java:12)\nCaused by: java.io.IOException: disk\n\t... 2 more", "plain"},
		},
	} {
		got := bodies(s.linesOf(t, test.stream, test.text))
		if strings.Join(got, "|") != strings.Join(test.want, "|") {
			t.Errorf("%s: joined into %q, want %q", test.stream, got, test.want)
		}
	}
}

// the examples in the comment on levelOf, and lines that name no level
func TestLinesFindTheLevelWhereProgramsWriteIt(t *testing.T) {
	fatal := slog.LevelError + 4
	for line, want := range map[string]*slog.Level{
		`{"level":50,"time":1727300000121,"msg":"failed"}`:              new(slog.LevelError),
		`{"level":"warn","msg":"slow"}`:                                 new(slog.LevelWarn),
		`level=warn msg="slow request" ms=1200`:                         new(slog.LevelWarn),
		`ts=2026-09-26T12:00:01Z lvl=error msg=boom`:                    new(slog.LevelError),
		"E0926 12:00:01.000000  1 main.go:12] boom":                     new(slog.LevelError),
		"I20260923 00:47:32.100929   141 raft_server.h:60] ok":          new(slog.LevelInfo),
		"2026-09-26 12:00:01,500 ERROR [main] boom":                     new(slog.LevelError),
		"2026-09-21 21:09:00,391 - aiohttp.access - INFO - GET /health": new(slog.LevelInfo),
		"2026-09-11 13:14:49.437 UTC [48] LOG:  listening":              new(slog.LevelInfo),
		"2026-09-11 13:14:49.437 UTC [48] FATAL:  terminating":          &fatal,
		"2026/09/24 06:12:02.759278 [Warning] core: Xray started":       new(slog.LevelWarn),
		"1:M 11 Sep 2026 13:18:33.307 # WARNING Memory overcommit":      new(slog.LevelWarn),
		"2026/09/26 12:00:01 [error] 7#7: *1 open() failed":             new(slog.LevelError),
		"connection reset by peer":                                      nil,
		"no error occurred":                                             nil,
		"LOG rotated":                                                   nil,
	} {
		records := []Field(nil)
		if fields, ok := exactFields(line); ok {
			records = fields
		}
		level, ok := levelOf(line, records)
		if ok != (want != nil) || (ok && level != *want) {
			t.Errorf("%q: level %v %v, want %v", line, level, ok, want)
		}
	}
}

// a line's text comes back byte for byte, however it was cut into writes, and
// a JSON object becomes its fields only when they spell it again
func TestLinesKeepEveryByte(t *testing.T) {
	s := openRecords(t)
	lines := []string{"plain", "with a carriage return\r", "", "a\ttab", "\x00 and \xff", `{"a":1,"b":"x"}`, `{ "a": 1 }`}
	w := s.Lines("bytes")
	text := strings.Join(lines, "\n") + "\n"
	for i := 0; i < len(text); i += 3 {
		if _, err := io.WriteString(w, text[i:min(i+3, len(text))]); err != nil {
			t.Fatal(err)
		}
	}
	if err := w.Close(); err != nil {
		t.Fatal(err)
	}
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	got := s.readAll(t, Query{Streams: []string{"bytes"}})
	var spelled []string
	for _, record := range got {
		if record.Body != nil {
			spelled = append(spelled, *record.Body)
			continue
		}
		var fields []string
		for _, field := range record.Attrs {
			fields = append(fields, fmt.Sprintf("%q:%s", field.Key, field.Value))
		}
		spelled = append(spelled, "{"+strings.Join(fields, ",")+"}")
	}
	if strings.Join(spelled, "\n") != strings.Join(lines, "\n") {
		t.Fatalf("lines came back as %q, want %q", spelled, lines)
	}
	if got[5].Body != nil || got[6].Body == nil {
		t.Fatalf("an exact object is its fields, a spaced one its text: %+v %+v", got[5], got[6])
	}
}

// a record that may still grow waits for a flush it did not grow through,
// and Close hands over the rest, a last line without its newline included
func TestLinesHandOverWhatWaitsOnceItStopsGrowing(t *testing.T) {
	s := openRecords(t)
	w := s.Lines("app")
	start := s.clock.Now()
	if _, err := io.WriteString(w, "2026-09-26 12:00:01,500 ERROR failed\n  at frame one\n"); err != nil {
		t.Fatal(err)
	}
	s.clock.advance(time.Second)
	if err := s.Flush(t.Context()); err != nil || len(s.readAll(t, Query{})) != 0 {
		t.Fatalf("a record still growing was handed over at the first flush: %v", err)
	}
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	got := s.readAll(t, Query{})
	if len(got) != 1 || !got[0].At.Equal(start) || got[0].Level == nil || *got[0].Level != slog.LevelError {
		t.Fatalf("after a flush it did not grow through: %+v", got)
	}
	if _, err := io.WriteString(w, "no newline yet"); err != nil {
		t.Fatal(err)
	}
	if err := w.Close(); err != nil {
		t.Fatal(err)
	}
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if got = s.readAll(t, Query{}); len(got) != 2 || *got[1].Body != "no newline yet" {
		t.Fatalf("after Close: %+v", got)
	}
	if _, err := io.WriteString(w, "late\n"); err == nil {
		t.Fatal("a closed writer took a line")
	}
}

// a writer of Lines never waits: what the buffer has no room for is dropped
// and counted, and the store's Close hands over what writers still hold
func TestLinesNeverWaitAndCloseWithTheStore(t *testing.T) {
	s := openTestStore(t, t.TempDir(), Options{Buffer: 4}, tinystore.Options{})
	w := s.Lines("busy")
	for i := range 10 {
		if _, err := fmt.Fprintf(w, "line %d\n", i); err != nil {
			t.Fatal(err)
		}
	}
	if stats := s.Stats(); stats.Dropped != 5 {
		t.Fatalf("dropped %d of nine handed over to a buffer of four", stats.Dropped)
	}
	if err := s.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	if stats := s.Stats(); stats.Appended != 5 {
		t.Fatalf("closing the store appended %d, want the four queued and the one held", stats.Appended)
	}
}

// a writer of Lines loses no byte: its records, one after another with a
// newline between them, are what it was given, however it was cut
func FuzzLinesLoseNoByte(f *testing.F) {
	f.Add("2026-09-26 12:00:01,500 ERROR failed\n  at frame\nnext\n{\n\"a\": [1,\n2]}\n", uint8(3))
	f.Add(`{"level":30,"msg":"ok"}`+"\nplain\r\n\n", uint8(1))
	f.Fuzz(func(t *testing.T, text string, cut uint8) {
		var got []string
		w := &lineWriter{stream: "fuzz", now: func() time.Time { return testNow }, release: func() {}}
		w.hand = func(record Record) { got = append(got, lineOf(&record)) }
		step := int(cut) + 1
		for i := 0; i < len(text); i += step {
			if _, err := io.WriteString(w, text[i:min(i+step, len(text))]); err != nil {
				t.Fatal(err)
			}
		}
		if err := w.Close(); err != nil {
			t.Fatal(err)
		}
		want := strings.TrimSuffix(text, "\n")
		if spelled := strings.Join(got, "\n"); spelled != want {
			t.Fatalf("records spell %q, want %q", spelled, want)
		}
	})
}

// lineOf writes a record of Lines as the line it came from
func lineOf(record *Record) string {
	if record.Body != nil {
		return *record.Body
	}
	var fields []string
	for _, field := range record.Attrs {
		fields = append(fields, string(appendJSONKey(nil, field.Key))+":"+field.Value)
	}
	return "{" + strings.Join(fields, ",") + "}"
}

// the examples in the comment on continues, and lines that begin records
func TestAStackTraceGoesOn(t *testing.T) {
	for line, want := range map[string]bool{
		"ValueError: bad input": true, "java.io.IOException: disk": true, "KeyError": true,
		"Caused by: java.io.IOException": true, "Traceback (most recent call last):": true,
		"goroutine 1 [running]:": true, "INFO started": false, "level=info msg=ok": false,
		`Exception in thread "main" java.lang.IllegalStateException: boom`: false, "error: no such file": false,
	} {
		if got := continues([]byte(line)); got != want {
			t.Errorf("%q goes on a stack trace: %v, want %v", line, got, want)
		}
	}
}

// BenchmarkLines is what a writer of Lines costs a line: text in the layouts
// stamps are found in, a JSON service's lines, and a traceback
func BenchmarkLines(b *testing.B) {
	var text strings.Builder
	for _, record := range textRecords(4096) {
		if record.Body != nil {
			text.WriteString(*record.Body)
		} else {
			text.WriteString(`{"level":30,"time":1727300000121,"msg":"request completed"}`)
		}
		text.WriteByte('\n')
	}
	text.WriteString("2026-09-26 12:00:01,500 ERROR failed\nTraceback (most recent call last):\n  File \"a.py\"\nValueError: x\n")
	lines := strings.Count(text.String(), "\n")
	b.SetBytes(int64(text.Len()))
	for b.Loop() {
		w := &lineWriter{
			stream: "bench", now: func() time.Time { return testNow }, hand: func(Record) {},
			release: func() {},
		}
		if _, err := io.WriteString(w, text.String()); err != nil {
			b.Fatal(err)
		}
		if err := w.Close(); err != nil {
			b.Fatal(err)
		}
	}
	b.ReportMetric(float64(b.N*lines)/b.Elapsed().Seconds(), "lines/s")
}
