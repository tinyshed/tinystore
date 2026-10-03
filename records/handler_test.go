package records

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"math"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

func TestBackgroundFlushSummaryDoesNotEnterRecordsHandler(t *testing.T) {
	s := openLogging(t, t.TempDir(), Options{})
	var output bytes.Buffer
	s.log = slog.New(slog.NewMultiHandler(
		slog.NewJSONHandler(&output, &slog.HandlerOptions{Level: slog.LevelDebug}), s.Handler("diagnostics", ConsoleOff),
	)).With("engine", "records")
	if err := s.flushInBackground(t.Context()); err != nil {
		t.Fatal(err)
	}
	if !bytes.Contains(output.Bytes(), []byte(`"msg":"flush finished"`)) ||
		!bytes.Contains(output.Bytes(), []byte(`"engine":"records"`)) {
		t.Fatalf("missing scoped Debug summary: %s", output.String())
	}
	if len(s.queue) != 0 {
		t.Fatal("records queued its own flush summary")
	}
}

func TestDroppedLinesAreSaidOncePerQuietPeriod(t *testing.T) {
	s := openLogging(t, t.TempDir(), Options{Buffer: 2})
	var output bytes.Buffer
	s.log = slog.New(slog.NewTextHandler(&output, &slog.HandlerOptions{
		ReplaceAttr: func(_ []string, attr slog.Attr) slog.Attr {
			if attr.Key == slog.TimeKey {
				return slog.Attr{}
			}
			return attr
		},
	}))
	logger := slog.New(s.Handler("app", ConsoleOff))
	flush := func() {
		if err := s.flushInBackground(t.Context()); err != nil {
			t.Fatal(err)
		}
	}
	flush()
	for range 5 { // two fit, three are dropped
		logger.Info("line")
	}
	flush()
	logger.Info("one")
	logger.Info("two")
	logger.Info("dropped")
	s.clock.advance(9 * time.Minute)
	flush()
	s.clock.advance(time.Minute)
	flush()
	flush()

	want := `level=WARN msg="log lines dropped" dropped=3 buffer=2` + "\n" +
		`level=WARN msg="log lines dropped" dropped=1 buffer=2` + "\n"
	if output.String() != want {
		t.Fatalf("said:\n%s\nwant:\n%s", output.String(), want)
	}
}

func TestAFailedFlushCountsItsDroppedRecordsByReason(t *testing.T) {
	s := openLogging(t, t.TempDir(), Options{})
	slog.New(s.Handler("app", ConsoleOff)).Info("waiting")
	err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, updateErr := tx.ExecContext(t.Context(), `create trigger refuse_head before insert on heads begin select raise(abort,'refused flush'); end`)
		return updateErr
	})
	if err != nil {
		t.Fatal(err)
	}
	if err = s.Flush(t.Context()); err == nil {
		t.Fatal("flush ignored the injected writer failure")
	}
	if stats := s.Stats(); stats.Dropped != 1 || stats.DroppedWrite != 1 ||
		stats.DroppedFull != 0 || stats.DroppedInvalid != 0 {
		t.Fatalf("failed flush: %+v", stats)
	}
	err = s.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, updateErr := tx.ExecContext(t.Context(), `drop trigger refuse_head`)
		return updateErr
	})
	if err != nil {
		t.Fatal(err)
	}
}

// openLogging opens a store on the wall clock, the one slog stamps its lines with
func openLogging(t *testing.T, dir string, options Options) *testStore {
	t.Helper()
	s := openTestStore(t, dir, options, tinystore.Options{})
	s.clock.set(time.Now())
	return s
}

// a line becomes a record: the message its body, logger.With its context,
// the call's attributes its attributes, a group's keys prefixed
func TestLogsLandInRecordsWithTheirAttributes(t *testing.T) {
	s := openLogging(t, t.TempDir(), Options{})
	logger := slog.New(s.Handler("notes", ConsoleOff)).With("service", "notes", "request_id", "r-17")
	logger.WithGroup("http").Warn("slow request", "route", "/notes", "ms", 1200, "error", errors.New("timeout"),
		slog.Group("user", "id", 7), "ratio", math.NaN(), "took", 1500*time.Millisecond)
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}

	got := s.readAll(t, Query{})
	if len(got) != 1 {
		t.Fatalf("read %d records", len(got))
	}
	record := got[0]
	if record.Stream != "notes" || record.Name != "log" || *record.Body != "slow request" || *record.Level != slog.LevelWarn {
		t.Fatalf("read %+v", record)
	}
	wantContext := []Field{{"service", `"notes"`}, {"request_id", `"r-17"`}}
	wantAttrs := []Field{
		{"http.route", `"/notes"`},
		{"http.ms", "1200"},
		{"http.error", `"timeout"`},
		{"http.user.id", "7"},
		{"http.ratio", `"NaN"`},
		{"http.took", "1500000000"},
	}
	if differ(&Record{
		At: record.At, Stream: "notes", Name: "log", Level: record.Level, Body: record.Body,
		Context: wantContext, Attrs: wantAttrs,
	}, &record) != "" {
		t.Fatalf("context %v, attributes %v", record.Context, record.Attrs)
	}
}

func TestAFullBufferDropsAndCountsWithoutWaiting(t *testing.T) {
	s := openLogging(t, t.TempDir(), Options{Buffer: 2})
	logger := slog.New(s.Handler("app", ConsoleOff))
	for range 5 {
		logger.Info("line")
	}
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if stats := s.Stats(); stats.Appended != 2 || stats.Dropped != 3 || stats.DroppedFull != 3 ||
		stats.DroppedInvalid != 0 || stats.DroppedWrite != 0 {
		t.Fatalf("stats %+v", stats)
	}
}

func TestTheEnginesOwnLinesAreRefused(t *testing.T) {
	s := openLogging(t, t.TempDir(), Options{})
	slog.New(s.Handler("app", ConsoleOff)).With("engine", "records").Info("flushed")
	slog.New(s.Handler("app", ConsoleOff)).Info("flushed", "engine", "records")
	slog.New(s.Handler("app", ConsoleOff)).With("engine", "metrics").Info("opened")
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if got := s.readAll(t, Query{}); len(got) != 1 || *got[0].Body != "opened" {
		t.Fatalf("read %+v", got)
	}
}

// a line the format cannot keep, or from past the store's clock and its skew,
// is dropped and counted, as a full buffer's is
func TestALineOutOfBoundsIsDroppedAndCounted(t *testing.T) {
	s := openLogging(t, t.TempDir(), Options{})
	slog.New(s.Handler("app", ConsoleOff)).Info(string(make([]byte, maxBlockInput)))
	slog.New(s.Handler("", ConsoleOff)).Info("no stream")
	ahead := slog.NewRecord(s.clock.Now().Add(time.Hour), slog.LevelInfo, "from an hour ahead", 0)
	if err := s.Handler("app", ConsoleOff).Handle(t.Context(), ahead); err != nil {
		t.Fatal(err)
	}
	slog.New(s.Handler("app", ConsoleOff)).Info("kept")
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if stats := s.Stats(); stats.Dropped != 3 || stats.DroppedInvalid != 3 || stats.Appended != 1 {
		t.Fatalf("stats %+v", stats)
	}
}

func TestClosingTheStoreWritesWhatIsWaiting(t *testing.T) {
	dir := t.TempDir()
	s := openLogging(t, dir, Options{})
	slog.New(s.Handler("app", ConsoleOff)).Info("last words")
	if err := s.runtime.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	reopened := openTestStore(t, dir, Options{}, tinystore.Options{})
	if got := reopened.readAll(t, Query{}); len(got) != 1 || *got[0].Body != "last words" {
		t.Fatalf("after close and reopen: %+v", got)
	}
}

func BenchmarkHandler(b *testing.B) {
	store, err := tinystore.Open(b.Context(), b.TempDir(), tinystore.Options{Manual: true})
	if err != nil {
		b.Fatal(err)
	}
	defer store.Close(context.Background())
	logs, err := Open(b.Context(), store, Options{Buffer: 1 << 20})
	if err != nil {
		b.Fatal(err)
	}
	logger := slog.New(logs.Handler("app", ConsoleOff)).With("service", "api")
	for b.Loop() {
		logger.Info("request finished", "route", "/notes", "status", 200, "ms", 12)
	}
}

// A buffer half full asks for its flush before the interval, so that bursts
// the buffer could not hold between two intervals are written, not dropped.
func TestAHalfFullBufferFlushesBeforeItsInterval(t *testing.T) {
	ctx := t.Context()
	store, err := tinystore.Open(ctx, t.TempDir(), tinystore.Options{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if closeErr := store.Close(context.WithoutCancel(ctx)); closeErr != nil {
			t.Error(closeErr)
		}
	})
	s, err := Open(ctx, store, Options{Buffer: 64, Flush: time.Hour})
	if err != nil {
		t.Fatal(err)
	}
	logger := slog.New(s.Handler("app", ConsoleOff))
	var written uint64
	for range 8 {
		for range 32 {
			logger.Info("line")
		}
		written += 32
		for deadline := time.Now().Add(5 * time.Second); s.Stats().Appended < written && time.Now().Before(deadline); {
			time.Sleep(time.Millisecond)
		}
	}
	if stats := s.Stats(); stats.Appended != written || stats.Dropped != 0 {
		t.Fatalf("%d lines in bursts of half the buffer, an hour's flush: %+v", written, stats)
	}
}

// writeTo sends a handler's console lines to a test's writer, in UTC
type writeTo struct{ io.Writer }

func (w writeTo) handlerOption(s *handlerSettings) { s.out = w.Writer }

// a line reaches its console as it is logged, before any flush: pretty with a
// duration as a person reads it, JSON with the nanoseconds the record keeps
func TestEachLineReachesTheConsoleAsItIsLogged(t *testing.T) {
	s := openLogging(t, t.TempDir(), Options{})
	var pretty, lines bytes.Buffer
	line := slog.NewRecord(time.Date(2026, 10, 2, 11, 2, 11, 123_000_000, time.UTC), slog.LevelWarn, "slow request", 0)
	line.AddAttrs(slog.Duration("took", 1500*time.Millisecond))
	with := []slog.Attr{slog.String("requestId", "7f3a")}
	for _, handler := range []slog.Handler{
		s.Handler("api", ConsolePretty, writeTo{&pretty}).WithAttrs(with),
		s.Handler("api", ConsoleJSON, writeTo{&lines}).WithAttrs(with),
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

// the engine's own lines are no loop on the console, so they reach it from
// Info up; the store still refuses them
func TestTheEnginesOwnLinesReachTheConsoleAndNotTheStore(t *testing.T) {
	s := openLogging(t, t.TempDir(), Options{})
	var console bytes.Buffer
	logger := slog.New(s.Handler("app", ConsoleJSON, writeTo{&console}))
	logger.With("engine", "records").Info("flushed")
	logger.Info("sealed", "engine", "records")
	logger.With("engine", "records").Debug("flush finished")
	logger.Debug("flush finished", "engine", "records")
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if got := s.readAll(t, Query{}); len(got) != 0 {
		t.Fatalf("the store kept %+v", got)
	}
	if lines := strings.Count(console.String(), "\n"); lines != 2 {
		t.Fatalf("the console has %d lines: %s", lines, console.String())
	}
}

func TestARedactedFieldIsHiddenInTheStoreAndOnTheConsole(t *testing.T) {
	s := openLogging(t, t.TempDir(), Options{})
	var console bytes.Buffer
	logger := slog.New(s.Handler("api", ConsoleJSON, writeTo{&console}, Redact("password", "authorization")))
	logger.With("token", "t-1").WithGroup("req").Info("login", "Authorization", "Bearer x",
		"user", map[string]any{"name": "ann", "password": "y"}, "password", "hunter2")
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}

	got := s.readAll(t, Query{})
	if len(got) != 1 {
		t.Fatalf("read %d records", len(got))
	}
	wantAttrs := []Field{
		{"req.Authorization", `"[redacted]"`},
		{"req.user", `{"name":"ann","password":"[redacted]"}`},
		{"req.password", `"[redacted]"`},
	}
	if !slices.Equal(got[0].Attrs, wantAttrs) || !slices.Equal(got[0].Context, []Field{{"token", `"t-1"`}}) {
		t.Fatalf("context %v, attributes %v", got[0].Context, got[0].Attrs)
	}
	for _, secret := range []string{"Bearer x", `"y"`, "hunter2"} {
		if strings.Contains(console.String(), secret) {
			t.Errorf("the console shows %s: %s", secret, console.String())
		}
	}
}

func TestALevelKeepsALoggersLinesFromItUp(t *testing.T) {
	s := openLogging(t, t.TempDir(), Options{})
	var console bytes.Buffer
	logger := slog.New(s.Handler("api", Level(slog.LevelWarn), ConsoleJSON, writeTo{&console}))
	logger.Debug("d")
	logger.Info("i")
	logger.Warn("w")
	logger.Error("e")
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	got := s.readAll(t, Query{})
	if len(got) != 2 || *got[0].Body != "w" || *got[1].Body != "e" {
		t.Fatalf("read %+v", got)
	}
	if lines := strings.Count(console.String(), "\n"); lines != 2 {
		t.Fatalf("the console has %d lines: %s", lines, console.String())
	}
}

// lines logged from many goroutines at once each reach the console whole
func TestConsoleLinesFromManyGoroutinesDoNotInterleave(t *testing.T) {
	s := openLogging(t, t.TempDir(), Options{Buffer: 1})
	var console bytes.Buffer
	logger := slog.New(s.Handler("api", ConsoleJSON, writeTo{&console})).With("worker", "w")
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

// a logger without a store is the console's alone: its lines are written as
// a store's handler writes them, and none is queued
func TestAHandlerWithoutAStoreWritesTheConsoleAlone(t *testing.T) {
	var console bytes.Buffer
	logger := slog.New(Handler("app", ConsolePretty, writeTo{&console}, Redact("password"))).With("module", "billing")
	logger.Info("charged", "password", "hunter2", "amount", 25)
	if want := " INFO  app  charged  module=billing password=[redacted] amount=25\n"; !strings.HasSuffix(console.String(), want) {
		t.Fatalf("the console has %q, want it to end %q", console.String(), want)
	}
	if Handler("app", ConsoleOff).Enabled(t.Context(), slog.LevelError) {
		t.Fatal("a handler with neither a store nor a console takes lines")
	}
}
