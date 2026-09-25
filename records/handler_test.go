package records

import (
	"context"
	"errors"
	"log/slog"
	"math"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

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
	logger := slog.New(s.Handler("notes")).With("service", "notes", "request_id", "r-17")
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
	logger := slog.New(s.Handler("app"))
	for range 5 {
		logger.Info("line")
	}
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if stats := s.Stats(); stats.Appended != 2 || stats.Dropped != 3 {
		t.Fatalf("stats %+v", stats)
	}
}

func TestTheEnginesOwnLinesAreRefused(t *testing.T) {
	s := openLogging(t, t.TempDir(), Options{})
	slog.New(s.Handler("app")).With("engine", "records").Info("flushed")
	slog.New(s.Handler("app")).Info("flushed", "engine", "records")
	slog.New(s.Handler("app")).With("engine", "metrics").Info("opened")
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
	slog.New(s.Handler("app")).Info(string(make([]byte, maxBlockInput)))
	slog.New(s.Handler("")).Info("no stream")
	ahead := slog.NewRecord(s.clock.Now().Add(time.Hour), slog.LevelInfo, "from an hour ahead", 0)
	if err := s.Handler("app").Handle(t.Context(), ahead); err != nil {
		t.Fatal(err)
	}
	slog.New(s.Handler("app")).Info("kept")
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if stats := s.Stats(); stats.Dropped != 3 || stats.Appended != 1 {
		t.Fatalf("stats %+v", stats)
	}
}

func TestClosingTheStoreWritesWhatIsWaiting(t *testing.T) {
	dir := t.TempDir()
	s := openLogging(t, dir, Options{})
	slog.New(s.Handler("app")).Info("last words")
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
	logger := slog.New(logs.Handler("app")).With("service", "api")
	for b.Loop() {
		logger.Info("request finished", "route", "/notes", "status", 200, "ms", 12)
	}
}
