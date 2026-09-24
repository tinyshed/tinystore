package records

import (
	"context"
	"errors"
	"log/slog"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

var testNow = time.Date(2026, 9, 24, 12, 0, 0, 0, time.UTC)

func openTestRecords(t *testing.T, dir string, options Options) (*Store, *tinystore.Store) {
	t.Helper()
	store, err := tinystore.Open(t.Context(), dir, tinystore.Options{Manual: true, Clock: func() time.Time { return testNow }})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close(context.Background()) })
	logs, err := Open(t.Context(), store, options)
	if err != nil {
		t.Fatal(err)
	}
	return logs, store
}

func readAll(t *testing.T, logs *Store) []Record {
	t.Helper()
	got, err := logs.Read(t.Context(), Query{From: time.Unix(0, 0), To: time.Now().Add(time.Hour), MinLevel: slog.LevelDebug})
	if err != nil {
		t.Fatal(err)
	}
	return got
}

func TestLogsLandInRecordsWithTheirAttributes(t *testing.T) {
	logs, _ := openTestRecords(t, t.TempDir(), Options{})
	logger := slog.New(logs.Handler()).With("service", "notes")
	logger.WithGroup("http").Warn("slow request", "route", "/notes", "ms", 1200, "error", errors.New("timeout"))
	if err := logs.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}

	got := readAll(t, logs)
	if len(got) != 1 || got[0].Message != "slow request" || got[0].Level != slog.LevelWarn {
		t.Fatalf("read %+v", got)
	}
	want := map[string]any{"service": "notes", "http.route": "/notes", "http.ms": float64(1200), "http.error": "timeout"}
	for key, value := range want {
		if got[0].Attrs[key] != value {
			t.Errorf("%s = %v, want %v", key, got[0].Attrs[key], value)
		}
	}
}

func TestAFullBufferDropsAndCountsWithoutWaiting(t *testing.T) {
	logs, _ := openTestRecords(t, t.TempDir(), Options{Buffer: 2})
	logger := slog.New(logs.Handler())
	for range 5 {
		logger.Info("line")
	}
	if err := logs.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if stats := logs.Stats(); stats.Written != 2 || stats.Dropped != 3 {
		t.Fatalf("stats %+v", stats)
	}
}

func TestTheEnginesOwnLinesAreRefused(t *testing.T) {
	logs, _ := openTestRecords(t, t.TempDir(), Options{})
	slog.New(logs.Handler()).With("engine", "records").Info("flushed")
	slog.New(logs.Handler()).Info("flushed", "engine", "records")
	slog.New(logs.Handler()).With("engine", "metrics").Info("opened")
	if err := logs.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if got := readAll(t, logs); len(got) != 1 || got[0].Message != "opened" {
		t.Fatalf("read %+v", got)
	}
}

func TestReadFiltersByTimeAndLevelAndIsBounded(t *testing.T) {
	logs, _ := openTestRecords(t, t.TempDir(), Options{})
	handler := logs.Handler()
	for i, level := range []slog.Level{slog.LevelDebug, slog.LevelInfo, slog.LevelError} {
		record := slog.NewRecord(testNow.Add(time.Duration(i)*time.Minute), level, level.String(), 0)
		if err := handler.Handle(t.Context(), record); err != nil {
			t.Fatal(err)
		}
	}
	if err := logs.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}

	got, err := logs.Read(t.Context(), Query{From: testNow, To: testNow.Add(2 * time.Minute), MinLevel: slog.LevelInfo})
	if err != nil || len(got) != 1 || got[0].Message != "INFO" {
		t.Fatalf("info and above in the first two minutes: %+v, %v", got, err)
	}
	got, err = logs.Read(t.Context(), Query{From: testNow, To: testNow.Add(time.Hour), MinLevel: slog.LevelDebug, Limit: 2})
	if err != nil || len(got) != 2 || got[0].Message != "DEBUG" {
		t.Fatalf("the oldest two: %+v, %v", got, err)
	}
	if _, err = logs.Read(t.Context(), Query{To: testNow, Limit: 10_001}); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("an unbounded read: %v", err)
	}
}

func TestRetentionRemovesOldRecords(t *testing.T) {
	logs, _ := openTestRecords(t, t.TempDir(), Options{Retention: time.Hour})
	handler := logs.Handler()
	for _, at := range []time.Time{testNow.Add(-2 * time.Hour), testNow.Add(-time.Minute)} {
		if err := handler.Handle(t.Context(), slog.NewRecord(at, slog.LevelInfo, "line", 0)); err != nil {
			t.Fatal(err)
		}
	}
	if err := logs.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := logs.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	if got := readAll(t, logs); len(got) != 1 || logs.Stats().Expired != 1 {
		t.Fatalf("after retention: %+v, %+v", got, logs.Stats())
	}
}

func TestClosingTheStoreWritesWhatIsWaiting(t *testing.T) {
	dir := t.TempDir()
	logs, store := openTestRecords(t, dir, Options{})
	slog.New(logs.Handler()).Info("last words")
	if err := store.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	reopened, _ := openTestRecords(t, dir, Options{})
	if got := readAll(t, reopened); len(got) != 1 || got[0].Message != "last words" {
		t.Fatalf("after close and reopen: %+v", got)
	}
}
