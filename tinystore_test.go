package tinystore

import (
	"context"
	"errors"
	"slices"
	"sync"
	"testing"
	"time"
)

func openTestStore(t *testing.T, dir string, options Options) *Store {
	t.Helper()
	store, err := Open(t.Context(), dir, options)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := store.Close(context.Background()); err != nil {
			t.Error(err)
		}
	})
	return store
}

func TestASecondStoreOnTheSameDirectoryIsRefused(t *testing.T) {
	if !directoryLocking {
		t.Skip("no directory lock on this platform")
	}
	dir := t.TempDir()
	first, err := Open(t.Context(), dir, Options{})
	if err != nil {
		t.Fatal(err)
	}
	if _, err = Open(t.Context(), dir, Options{}); !errors.Is(err, ErrInUse) {
		t.Fatalf("second store: %v", err)
	}
	if err = first.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	openTestStore(t, dir, Options{})
}

type closingEngine struct {
	name   string
	closed *[]string
	mu     *sync.Mutex
}

func (e closingEngine) Close(context.Context) error {
	e.mu.Lock()
	defer e.mu.Unlock()
	*e.closed = append(*e.closed, e.name)
	return nil
}

func TestCloseClosesEnginesLastOpenedFirstAndOnlyOnce(t *testing.T) {
	store, err := Open(t.Context(), t.TempDir(), Options{})
	if err != nil {
		t.Fatal(err)
	}
	var closed []string
	var mu sync.Mutex
	for _, name := range []string{"metrics", "records", "sql"} {
		if err = store.Attach(closingEngine{name: name, closed: &closed, mu: &mu}); err != nil {
			t.Fatal(err)
		}
	}

	if err = store.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err = store.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	if want := []string{"sql", "records", "metrics"}; !slices.Equal(closed, want) {
		t.Fatalf("closed %v, want %v", closed, want)
	}
	if err = store.Attach(closingEngine{}); !errors.Is(err, ErrClosed) {
		t.Fatalf("attach after close: %v", err)
	}
	if _, _, err = store.Claim("kv.db"); !errors.Is(err, ErrClosed) {
		t.Fatalf("claim after close: %v", err)
	}
}

type slowEngine struct{ release chan struct{} }

func (e slowEngine) Close(context.Context) error {
	<-e.release
	return nil
}

func TestCloseReturnsAtItsDeadlineAndStillFinishes(t *testing.T) {
	store, err := Open(t.Context(), t.TempDir(), Options{})
	if err != nil {
		t.Fatal(err)
	}
	engine := slowEngine{release: make(chan struct{})}
	if err = store.Attach(engine); err != nil {
		t.Fatal(err)
	}

	short, cancel := context.WithTimeout(t.Context(), 10*time.Millisecond)
	defer cancel()
	if err = store.Close(short); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("close past its deadline: %v", err)
	}
	close(engine.release)
	if err = store.Close(t.Context()); err != nil {
		t.Fatalf("the cleanup that kept going: %v", err)
	}
}

func TestAnEmptyDirectoryNameIsRefused(t *testing.T) {
	if _, err := Open(t.Context(), "", Options{}); !errors.Is(err, ErrInvalid) {
		t.Fatal(err)
	}
}
