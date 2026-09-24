package tinystore

import (
	"bytes"
	"context"
	"errors"
	"log/slog"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func TestEveryRunsUntilTheStoreCloses(t *testing.T) {
	store, err := Open(t.Context(), t.TempDir(), Options{})
	if err != nil {
		t.Fatal(err)
	}
	var runs atomic.Int64
	store.Every("count", time.Millisecond, func(context.Context) error {
		runs.Add(1)
		return nil
	})
	deadline := time.Now().Add(5 * time.Second)
	for runs.Load() < 3 {
		if time.Now().After(deadline) {
			t.Fatalf("ran %d times in five seconds", runs.Load())
		}
		time.Sleep(time.Millisecond)
	}

	if err = store.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	after := runs.Load()
	time.Sleep(20 * time.Millisecond)
	if runs.Load() != after {
		t.Fatal("background work ran after Close returned")
	}
}

func TestAManualStoreRunsNoBackgroundWork(t *testing.T) {
	store := openTestStore(t, t.TempDir(), Options{Manual: true})
	var runs atomic.Int64
	store.Every("count", time.Millisecond, func(context.Context) error {
		runs.Add(1)
		return nil
	})
	time.Sleep(20 * time.Millisecond)
	if runs.Load() != 0 {
		t.Fatalf("a manual store ran work %d times", runs.Load())
	}
}

func TestBackgroundFailuresAreLoggedOncePerQuietPeriod(t *testing.T) {
	var out bytes.Buffer
	failures := &failureLog{
		logger: slog.New(slog.NewTextHandler(&out, &slog.HandlerOptions{
			ReplaceAttr: func(_ []string, attr slog.Attr) slog.Attr {
				if attr.Key == slog.TimeKey {
					return slog.Attr{}
				}
				return attr
			},
		})),
		work:  "maintenance",
		quiet: 10 * time.Minute,
	}
	start := time.Date(2026, 9, 24, 0, 0, 0, 0, time.UTC)
	full := errors.New("disk full")
	for minute := range 11 {
		failures.observe(start.Add(time.Duration(minute)*time.Minute), full)
	}
	failures.observe(start.Add(11*time.Minute), nil)
	failures.observe(start.Add(12*time.Minute), context.Canceled)

	want := strings.Join([]string{
		`level=WARN msg="background work failed" work=maintenance error="disk full" failures=1`,
		`level=WARN msg="background work failed" work=maintenance error="disk full" failures=11`,
		`level=INFO msg="background work recovered" work=maintenance failures=11`,
	}, "\n") + "\n"
	if out.String() != want {
		t.Fatalf("logged:\n%s\nwant:\n%s", out.String(), want)
	}
}
