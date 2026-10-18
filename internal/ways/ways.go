// Package ways opens a store each way a test asks for, in a directory of its
// own, closed and removed after: the core in this process, when
// TINYSTORE_LIBRARY names the core's library, and a private child, a sidecar
// and a remote server, when TINYSTORE_BIN names the tinystore binary.
package ways

import (
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// Token is a remote server's admin token, in a tokens file of the test's own.
const Token = "4dGQ6tR2kq0SxVZpLWn8E1yHf7cJmB3aUoN9Tz5Ki0E"

// Way is a way a store is reached.
type Way struct {
	Name  string
	Ready bool
	// Open opens the store a test's directory holds.
	Open func(t *testing.T, dir string) *tinystore.Store
	// Home is the store's own directory, where a test's directory holds more than the store.
	Home func(dir string) string
	// Shared says whether a second open of the directory joins the same store.
	Shared bool
}

func binary() string {
	found := os.Getenv("TINYSTORE_BIN")
	if found == "" || found == "unused" {
		return ""
	}
	if _, err := os.Stat(found); err != nil {
		return ""
	}
	return found
}

func library() string { return os.Getenv("TINYSTORE_LIBRARY") }

// All are the four ways.
func All() []Way {
	served := binary() != ""
	same := func(dir string) string { return dir }
	return []Way{
		{Name: "the core in this process", Ready: library() != "", Home: same, Shared: true,
			Open: func(t *testing.T, dir string) *tinystore.Store {
				return opened(t, dir, tinystore.Library(library()))
			}},
		{Name: "a private child", Ready: served, Home: same,
			Open: func(t *testing.T, dir string) *tinystore.Store {
				return opened(t, dir, tinystore.Private(), tinystore.Binary(binary()))
			}},
		{Name: "a sidecar", Ready: served, Home: same, Shared: true,
			Open: func(t *testing.T, dir string) *tinystore.Store {
				store := opened(t, dir, tinystore.Idle(time.Millisecond), tinystore.Binary(binary()))
				t.Cleanup(func() {
					until(func() bool { _, err := os.Stat(filepath.Join(dir, "server", "SERVE")); return err != nil })
				})
				return store
			}},
		{Name: "a remote server over TCP", Ready: served, Home: func(dir string) string { return filepath.Join(dir, "store") },
			Open: remote},
	}
}

func opened(t *testing.T, dir string, opts ...tinystore.Option) *tinystore.Store {
	t.Helper()
	store, err := tinystore.Open(context.Background(), dir, opts...)
	if err != nil {
		t.Fatalf("open %s: %v", dir, err)
	}
	return store
}

func remote(t *testing.T, dir string) *tinystore.Store {
	t.Helper()
	tokens := filepath.Join(dir, "tokens")
	if err := os.WriteFile(tokens, []byte("admin "+Token+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	served := filepath.Join(dir, "store")
	cmd := exec.Command(binary(), "serve", served, "--listen", "tcp://127.0.0.1:0", "--tokens", tokens)
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
	})
	serve := filepath.Join(served, "server", "SERVE")
	until(func() bool { _, err := os.Stat(serve); return err == nil })
	var published struct {
		Endpoints []string `json:"endpoints"`
	}
	text, err := os.ReadFile(serve)
	if err != nil || json.Unmarshal(text, &published) != nil || len(published.Endpoints) < 2 {
		t.Fatalf("SERVE of the remote server: %s %v", text, err)
	}
	store, err := tinystore.Connect(context.Background(), published.Endpoints[1], Token)
	if err != nil {
		t.Fatal(err)
	}
	return store
}

func until(done func() bool) {
	deadline := time.Now().Add(10 * time.Second)
	for !done() && time.Now().Before(deadline) {
		time.Sleep(20 * time.Millisecond)
	}
}

// Until waits up to ten seconds for done.
func Until(t *testing.T, done func() bool) {
	t.Helper()
	until(done)
	if !done() {
		t.Fatal("not done within ten seconds")
	}
}

// Each runs test through every way that is ready here, each with a store of its own.
func Each(t *testing.T, test func(t *testing.T, store *tinystore.Store)) {
	EachWay(t, func(t *testing.T, way Way) { test(t, Open(t, way)) })
}

// EachWay runs test through every way that is ready here, given the way itself.
func EachWay(t *testing.T, test func(t *testing.T, way Way)) {
	for _, way := range All() {
		t.Run(way.Name, func(t *testing.T) {
			if !way.Ready {
				t.Skip("not built here")
			}
			defer Recover(t)
			test(t, way)
		})
	}
}

// failed is an error Must met, which Recover fails the test with.
type failed struct{ err error }

// Must is a value, or a panic of its error that Recover makes the test's failure.
func Must[T any](value T, err error) T {
	if err != nil {
		panic(failed{err})
	}
	return value
}

// Must2 is two values, or a panic of their error that Recover makes the test's failure.
func Must2[A, B any](a A, b B, err error) (A, B) {
	if err != nil {
		panic(failed{err})
	}
	return a, b
}

// Recover fails the test with what Must met, deferred where the test runs.
func Recover(t *testing.T) {
	t.Helper()
	if r := recover(); r != nil {
		if f, ok := r.(failed); ok {
			t.Fatalf("%v", f.err)
		}
		panic(r)
	}
}

// Open opens a store the way says in a directory of the test's own, closed
// when the test ends.
func Open(t *testing.T, way Way) *tinystore.Store {
	t.Helper()
	dir := t.TempDir()
	store := way.Open(t, dir)
	t.Cleanup(func() { _ = store.Close() })
	return store
}

// Library is the core's library the tests load, "" when none is built.
func Library() string { return library() }

// Binary is the tinystore binary the tests run, "" when none is built.
func Binary() string { return binary() }
