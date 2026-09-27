package tinystore

import (
	"bytes"
	"context"
	"errors"
	"log/slog"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

type lockedLogBuffer struct {
	mu sync.Mutex
	bytes.Buffer
}

type failingEngine struct{ err error }

func (e failingEngine) Close(context.Context) error { return e.err }

func TestAFailedCloseDoesNotLogSuccess(t *testing.T) {
	var output lockedLogBuffer
	store, err := Open(t.Context(), t.TempDir(), Options{Logger: slog.New(slog.NewTextHandler(&output, nil))})
	if err != nil {
		t.Fatal(err)
	}
	failure := errors.New("engine file did not close")
	if err = store.Attach(failingEngine{err: failure}); err != nil {
		t.Fatal(err)
	}
	if err = store.Close(t.Context()); !errors.Is(err, failure) {
		t.Fatalf("Close: %v", err)
	}
	logged := output.String()
	if !strings.Contains(logged, "store closed with errors") || !strings.Contains(logged, failure.Error()) ||
		strings.Contains(logged, "msg=\"store closed\"") {
		t.Fatalf("wrong close outcome: %s", logged)
	}
}

func (b *lockedLogBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.Buffer.Write(p)
}

func (b *lockedLogBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.Buffer.String()
}

func TestEngineBackgroundFailuresCarryTheirOriginThroughRecovery(t *testing.T) {
	var output lockedLogBuffer
	logger := slog.New(slog.NewTextHandler(&output, &slog.HandlerOptions{
		ReplaceAttr: func(_ []string, attr slog.Attr) slog.Attr {
			if attr.Key == slog.TimeKey {
				return slog.Attr{}
			}
			return attr
		},
	}))
	store := openTestStore(t, t.TempDir(), Options{Logger: logger})
	var calls atomic.Int32
	recovered := make(chan struct{})
	store.EveryEngine("records", "records flush", time.Millisecond, func(context.Context) error {
		if call := calls.Add(1); call == 1 {
			return errors.New("disk full")
		} else if call == 2 {
			close(recovered)
		}
		return nil
	})
	select {
	case <-recovered:
	case <-time.After(5 * time.Second):
		t.Fatal("background work did not recover")
	}
	if err := store.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	logged := output.String()
	for line := range strings.SplitSeq(logged, "\n") {
		if (strings.Contains(line, "background work failed") || strings.Contains(line, "background work recovered")) &&
			(!strings.Contains(line, "engine=records") || !strings.Contains(line, "work=\"records flush\"")) {
			t.Fatalf("unscoped background event: %q", line)
		}
	}
	if !strings.Contains(logged, "background work failed") || !strings.Contains(logged, "background work recovered") {
		t.Fatalf("missing failure or recovery: %s", logged)
	}
}

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
