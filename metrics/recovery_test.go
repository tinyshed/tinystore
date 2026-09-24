package metrics

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"
)

func TestAbruptExitKeepsCommittedHeadAndGroups(t *testing.T) {
	for _, phase := range []string{"head", "sealed"} {
		t.Run(phase, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "crash.db")
			command := exec.CommandContext(t.Context(), os.Args[0], "-test.run=^TestAbruptExitHelper$")
			command.Env = append(os.Environ(), "TINYSTORE_CRASH_FILE="+path, "TINYSTORE_CRASH_PHASE="+phase)
			if output, err := command.CombinedOutput(); err != nil {
				t.Fatalf("child failed: %v\n%s", err, output)
			}
			store, err := Open(t.Context(), path, Options{})
			if err != nil {
				t.Fatal(err)
			}
			defer store.Close(context.Background())
			store.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
			assertSamples(t, readAll(t, store), testSamples(500))
		})
	}
}

func TestAbruptExitHelper(t *testing.T) {
	path := os.Getenv("TINYSTORE_CRASH_FILE")
	if path == "" {
		t.Skip("subprocess only")
	}
	store, err := Open(t.Context(), path, Options{})
	if err != nil {
		t.Fatal(err)
	}
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
	if err = store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: testSamples(500)}}); err != nil {
		t.Fatal(err)
	}
	if os.Getenv("TINYSTORE_CRASH_PHASE") == "sealed" {
		if _, err = store.Maintain(t.Context()); err != nil {
			t.Fatal(err)
		}
	}
	os.Exit(0)
}
