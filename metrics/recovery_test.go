package metrics

import (
	"context"
	"database/sql"
	"encoding/binary"
	"encoding/json"
	"io/fs"
	"math"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"testing/fstest"
	"time"

	"github.com/tinyshed/tinystore/codec"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

func TestSchemaOneFileUpgradesWithoutChangingSamples(t *testing.T) {
	ctx := t.Context()
	path := filepath.Join(t.TempDir(), "old.db")
	file, err := sqlite.Open(ctx, path, 2)
	if err != nil {
		t.Fatal(err)
	}
	initial, err := fs.ReadFile(migrationFiles, "migrations/0001_initial.sql")
	if err != nil {
		t.Fatal(err)
	}
	if err = file.Migrate(ctx, 0x544d4554, fstest.MapFS{"0001_initial.sql": &fstest.MapFile{Data: initial}}); err != nil {
		t.Fatal(err)
	}
	points := make([]Sample, 240)
	for i := range points {
		points[i] = Sample{At: testEpoch + int64(i), Value: 42}
	}
	c, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	head, body, err := c.Encode(points)
	if err != nil {
		t.Fatal(err)
	}
	group := blockGroup{seriesID: 1, start: head.Start, end: head.End, live: 1, blocks: []storedBlock{{head: head, summary: summarize(points, Gauge), body: body, bodyBytes: len(body)}}}
	directory, err := encodeDirectory(group)
	if err != nil {
		t.Fatal(err)
	}
	labels := []Label{{Name: "__name__", Value: "cpu"}}
	identity, err := json.Marshal(labels)
	if err != nil {
		t.Fatal(err)
	}
	legacyPoint := Sample{At: head.End + 1, Value: math.Copysign(0, -1)}
	if err = file.Update(ctx, func(tx *sql.Tx) error {
		if _, execErr := tx.ExecContext(ctx, `insert into series values(1,?, 'gauge')`, string(identity)); execErr != nil {
			return execErr
		}
		if _, execErr := tx.ExecContext(ctx, `insert into postings values('__name__','cpu',1)`); execErr != nil {
			return execErr
		}
		if _, execErr := tx.ExecContext(ctx, `insert into series_state(series_id,max_seen_ts,sealed_before,next_gc_ts,head_count) values(1,?,?,?,1)`, legacyPoint.At, head.End+1, head.End); execErr != nil {
			return execErr
		}
		if _, execErr := tx.ExecContext(ctx, `insert into head values(1,?,?)`, legacyPoint.At, binary.LittleEndian.AppendUint64(nil, math.Float64bits(legacyPoint.Value))); execErr != nil {
			return execErr
		}
		if _, execErr := tx.ExecContext(ctx, `update store_state set series_count=1 where id=1`); execErr != nil {
			return execErr
		}
		_, execErr := tx.ExecContext(ctx, `insert into groups values(1,?,?,?)`, head.Start, head.End, directory)
		return execErr
	}); err != nil {
		t.Fatal(err)
	}
	if err = file.Close(); err != nil {
		t.Fatal(err)
	}
	store, err := Open(ctx, path, Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close(context.Background())
	store.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
	points = append(points, legacyPoint)
	assertSamples(t, readAll(t, store), points)
	points[len(points)-1].Value = 7
	if err = store.Ingest(ctx, []Batch{{Series: Series{Labels: labels, Kind: Gauge}, Samples: points[len(points)-1:]}}); err != nil {
		t.Fatal(err)
	}
	assertSamples(t, readAll(t, store), points)
}

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
