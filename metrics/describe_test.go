package metrics

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"
)

// A description belongs to a name: the next Describe replaces it, one without
// a unit or help removes it, and a name never described has none.
func TestADescriptionIsKeptByName(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	described := func(name string, want Description) {
		t.Helper()
		if got, err := store.Description(t.Context(), name); err != nil || got != want {
			t.Fatalf("%s: %+v, want %+v: %v", name, got, want, err)
		}
	}
	if err := store.Describe(t.Context(), "query_ms", Unit("ms"), Help("How long a query took.")); err != nil {
		t.Fatal(err)
	}
	described("query_ms", Description{Unit: "ms", Help: "How long a query took."})
	if err := store.Describe(t.Context(), "query_ms", Unit("s")); err != nil {
		t.Fatal(err)
	}
	described("query_ms", Description{Unit: "s"})
	if err := store.Describe(t.Context(), "query_ms"); err != nil {
		t.Fatal(err)
	}
	described("query_ms", Description{})
	described("never_described", Description{})

	for _, refused := range []struct {
		name    string
		options []DescribeOption
	}{
		{"", []DescribeOption{Unit("ms")}},
		{"query_ms", []DescribeOption{Unit(strings.Repeat("x", maxUnitBytes+1))}},
		{"query_ms", []DescribeOption{Help(strings.Repeat("x", maxHelpBytes+1))}},
		{"query_ms", []DescribeOption{Help("\xff")}},
	} {
		if err := store.Describe(t.Context(), refused.name, refused.options...); !errors.Is(err, ErrInvalid) {
			t.Fatalf("a description of %q past its bounds: %v", refused.name, err)
		}
	}
}

// An instrument describes the names it writes at its first flush: a timer's
// sum and longest are milliseconds, its count has no unit.
func TestAnInstrumentDescribesItsSeries(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	store.Counter("requests_total", Help("Requests served.")).Inc()
	store.Gauge("inflight", Unit("requests")).Set(3)
	store.Timer("query", Help("Database queries.")).Record(2 * time.Millisecond)
	if err := store.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	for name, want := range map[string]Description{
		"requests_total": {Help: "Requests served."},
		"inflight":       {Unit: "requests"},
		"query_count":    {Help: "Database queries."},
		"query_sum":      {Unit: "ms", Help: "Database queries."},
		"query_max":      {Unit: "ms", Help: "Database queries."},
	} {
		if got, err := store.Description(t.Context(), name); err != nil || got != want {
			t.Fatalf("%s: %+v, want %+v: %v", name, got, want, err)
		}
	}
}

// A flush that fails keeps the descriptions it took for the next.
func TestAFailedFlushKeepsItsDescriptionsForTheNext(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	store.Counter("requests_total", Help("Requests served.")).Inc()
	cancelled, cancel := context.WithCancel(t.Context())
	cancel()
	if err := store.Flush(cancelled); !errors.Is(err, context.Canceled) {
		t.Fatalf("a flush whose context ended: %v", err)
	}
	if err := store.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if got, err := store.Description(t.Context(), "requests_total"); err != nil || got.Help != "Requests served." {
		t.Fatalf("a description after a failed flush: %+v: %v", got, err)
	}
}
