package tinystore_test

// What Open says when it cannot reach a store, through the core in this
// process, a private child and a sidecar: a directory that is a file, a
// binary that is not there, and a sidecar that fails before it serves.

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/ways"
)

// standIn makes this test binary stand in for tinystore: started as a
// sidecar, it fails and says why on stderr, writing no log.
const standIn = "TINYSTORE_TEST_STAND_IN"

func TestMain(m *testing.M) {
	if os.Getenv(standIn) != "" {
		fmt.Fprintln(os.Stderr, "tinystore serve: a stand-in that never serves")
		os.Exit(1)
	}
	os.Exit(m.Run())
}

type opening struct {
	name    string
	ready   bool
	options []tinystore.Option
}

// local are the ways a store opens from its directory: every one but a remote server's.
func local() []opening {
	library := os.Getenv("TINYSTORE_LIBRARY")
	binary := ways.Binary()
	return []opening{
		{"the core in this process", library != "", []tinystore.Option{tinystore.Library(library)}},
		{"a private child", binary != "", []tinystore.Option{tinystore.Private(), tinystore.Binary(binary)}},
		{"a sidecar", binary != "", []tinystore.Option{tinystore.Binary(binary)}},
	}
}

// contract: ERR-03, WIRE-01
func TestADirectoryThatIsAFileIsInvalidNamingTheFileInEveryWay(t *testing.T) {
	file := filepath.Join(t.TempDir(), "data")
	if err := os.WriteFile(file, []byte("not a directory"), 0o600); err != nil {
		t.Fatal(err)
	}
	under := filepath.Join(file, "store")
	for _, way := range local() {
		t.Run(way.name, func(t *testing.T) {
			if !way.ready {
				t.Skip("not built here")
			}
			_, err := tinystore.Open(context.Background(), file, way.options...)
			if want := "store " + file + " is a file, not a directory"; !errors.Is(err, tinystore.ErrInvalid) || err.Error() != want {
				t.Fatalf("got %v, want an invalid error %q", err, want)
			}
			_, err = tinystore.Open(context.Background(), under, way.options...)
			if want := "store " + under + ": " + file + " is a file, not a directory"; !errors.Is(err, tinystore.ErrInvalid) || err.Error() != want {
				t.Fatalf("got %v, want an invalid error %q", err, want)
			}
		})
	}
}

// contract: ERR-02
func TestABinaryThatIsNotThereIsNamedForASidecarAndAPrivateChild(t *testing.T) {
	base := t.TempDir()
	missing := filepath.Join(base, "nope")
	if runtime.GOOS == "windows" {
		missing += ".exe"
	}
	dir := filepath.Join(base, "store")
	_, err := tinystore.Open(context.Background(), dir, tinystore.Binary(missing))
	if want := "cannot start the sidecar " + missing + ": not found"; !errors.Is(err, tinystore.ErrClosed) || err.Error() != want {
		t.Fatalf("got %v, want a closed error %q", err, want)
	}
	_, err = tinystore.Open(context.Background(), dir, tinystore.Private(), tinystore.Binary(missing))
	if want := "cannot start the private server " + missing + ": not found"; !errors.Is(err, tinystore.ErrClosed) || err.Error() != want {
		t.Fatalf("got %v, want a closed error %q", err, want)
	}
}

// contract: ERR-02
func TestASidecarThatFailsBeforeItServesSaysWhatItWroteToStderr(t *testing.T) {
	dir := t.TempDir()
	if err := os.MkdirAll(filepath.Join(dir, "server"), 0o700); err != nil {
		t.Fatal(err)
	}
	stale := `{"level":"INFO","msg":"stopped","took":"0.1s"}` + "\n"
	if err := os.WriteFile(filepath.Join(dir, "server", "serve.log"), []byte(stale), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv(standIn, "1")
	stand, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	_, err = tinystore.Open(context.Background(), dir, tinystore.Binary(stand))
	want := "the sidecar exited with 1: tinystore serve: a stand-in that never serves"
	if !errors.Is(err, tinystore.ErrClosed) || err.Error() != want {
		t.Fatalf("got %v, want a closed error %q, not an older run's log", err, want)
	}
}

// contract: ERR-02
func TestAPrivateChildThatExitsBeforeItAnswersSaysWhatItWroteOnce(t *testing.T) {
	t.Setenv(standIn, "1")
	stand, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	_, err = tinystore.Open(context.Background(), t.TempDir(), tinystore.Private(), tinystore.Binary(stand))
	want := "the private server did not start: the private server exited with 1: tinystore serve: a stand-in that never serves"
	if !errors.Is(err, tinystore.ErrClosed) || err.Error() != want {
		t.Fatalf("got %v, want a closed error %q", err, want)
	}
}
