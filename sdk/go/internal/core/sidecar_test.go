package core

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"
)

// standIn makes this test binary stand in for tinystore started as a
// sidecar, which never serves: the variable's value says what it does.
const standIn = "TINYSTORE_TEST_CORE_STAND_IN"

func TestMain(m *testing.M) {
	switch os.Getenv(standIn) {
	case "":
		os.Exit(m.Run())
	case "waits":
		// a sidecar slow to start, which leaves soon after the test that started it
		time.Sleep(1500 * time.Millisecond)
	case "logs":
		// one that fails once its log is open: it says why there alone
		if at := slices.Index(os.Args, "--log-file"); at >= 0 && at+1 < len(os.Args) {
			log, err := os.OpenFile(os.Args[at+1], os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o600)
			if err == nil {
				_, _ = log.WriteString(`{"level":"ERROR","msg":"no space left"}` + "\n")
				_ = log.Close()
			}
		}
	}
	os.Exit(1)
}

func standingIn(t *testing.T, as string) func(context.Context) (string, error) {
	t.Helper()
	t.Setenv(standIn, as)
	stand, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	return func(context.Context) (string, error) { return stand, nil }
}

// contract: WIRE-11
func TestASidecarWhoseReleaseWasFetchedSaysItStartsOnceItDoes(t *testing.T) {
	stand := standingIn(t, "waits")
	link := NewLink(Sidecar(t.TempDir(), func(ctx context.Context) (string, error) {
		Waiting(ctx, "tinystore 0.1.0 downloads")
		return stand(ctx)
	}, Started{}))
	defer link.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()
	if _, err := link.Connection(ctx); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("got %v, want the deadline", err)
	}
	if said := link.Waiting(); said != "the sidecar starts" {
		t.Fatalf("a dial given up on while its sidecar starts waited while %q", said)
	}
}

// contract: ERR-02
func TestASidecarThatWroteNothingToStderrIsToldByTheLinesItsLogGainedOrByNone(t *testing.T) {
	for _, c := range []struct{ as, said string }{
		{"logs", `{"level":"ERROR","msg":"no space left"}`},
		{"silent", "it wrote no log"},
	} {
		t.Run(c.as, func(t *testing.T) {
			binary := standingIn(t, c.as)
			dir := t.TempDir()
			if err := os.MkdirAll(filepath.Join(dir, "server"), 0o700); err != nil {
				t.Fatal(err)
			}
			older := `{"level":"INFO","msg":"stopped"}` + "\n"
			if err := os.WriteFile(filepath.Join(dir, "server", "serve.log"), []byte(older), 0o600); err != nil {
				t.Fatal(err)
			}
			link := NewLink(Sidecar(dir, binary, Started{}))
			defer link.Close()
			_, err := link.Connection(context.Background())
			if want := "the sidecar exited with 1: " + c.said; KindOf(err) != Closed || err.Error() != want {
				t.Fatalf("got %v, want a closed error %q", err, want)
			}
		})
	}
}
