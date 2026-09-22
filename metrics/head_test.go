package metrics

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"math"
	"math/rand/v2"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestPackedHeadReplacementsAndByteBudget(t *testing.T) {
	store, _ := openTestStore(t, Options{MaxHeadBytes: 96})
	first := []Sample{{At: testEpoch, Value: math.Copysign(0, -1)}, {At: testEpoch + 2, Value: 1}}
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: first}}); err != nil {
		t.Fatal(err)
	}
	patch := []Sample{{At: testEpoch + 2, Value: math.Float64frombits(0x7ff8000000001234)}, {At: testEpoch + 1, Value: math.Inf(1)}}
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: patch}}); err != nil {
		t.Fatal(err)
	}
	want := []Sample{first[0], patch[1], patch[0]}
	assertSamples(t, readAll(t, store), want)
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: testSamples(200)}}); !errors.Is(err, ErrLimit) {
		t.Fatalf("packed byte bound: %v", err)
	}
	assertSamples(t, readAll(t, store), want)
	if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
		var rows int
		err := tx.QueryRowContext(t.Context(), `select count(*) from head`).Scan(&rows)
		if rows != 0 {
			t.Fatal("new ingest still writes per-sample rows")
		}
		return err
	}); err != nil {
		t.Fatal(err)
	}
}

func TestLegacyHeadConvertsOnFirstMutation(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	points := testSamples(10)
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if err := store.file.Update(t.Context(), func(tx *sql.Tx) error {
		for _, p := range points {
			if _, err := tx.ExecContext(t.Context(), `insert into head values(1,?,?)`, p.At, binary.LittleEndian.AppendUint64(nil, math.Float64bits(p.Value))); err != nil {
				return err
			}
		}
		_, err := tx.ExecContext(t.Context(), `update series_state set tail=null where series_id=1`)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	assertSamples(t, readAll(t, store), points)
	points[2].Value = 42
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points[2:3]}}); err != nil {
		t.Fatal(err)
	}
	assertSamples(t, readAll(t, store), points)
	if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
		var legacy, packed int
		err := tx.QueryRowContext(t.Context(), `select (select count(*) from head),(select count(*) from series_state where tail is not null)`).Scan(&legacy, &packed)
		if legacy != 0 || packed != 1 {
			t.Fatal("head not migrated")
		}
		return err
	}); err != nil {
		t.Fatal(err)
	}
}

func FuzzPackedHead(f *testing.F) {
	s := encodingStore(f)
	s.opts.MaxHeadSamples = 1000
	s.opts.MaxHeadBytes = 16384
	points := []Sample{{At: 1, Value: math.Copysign(0, -1)}, {At: 3, Value: 42}}
	encoded, err := s.encodeHead(context.Background(), 1, points)
	if err != nil {
		f.Fatal(err)
	}
	f.Add(encoded)
	f.Fuzz(func(t *testing.T, data []byte) {
		if len(data) > s.opts.MaxHeadBytes {
			return
		}
		data = append([]byte(nil), data...)
		if len(data) >= 4 {
			binary.LittleEndian.PutUint32(data[len(data)-4:], headChecksum(1, data[:len(data)-4]))
		}
		out, err := s.decodeHead(t.Context(), headSnapshot{seriesID: 1, count: 2, start: 1, end: 3, packed: data})
		if err == nil && len(out) != 2 {
			t.Fatal("unbounded head")
		}
	})
}

func TestHeadChecksumRejectsAnotherSeries(t *testing.T) {
	s := encodingStore(t)
	s.opts.MaxHeadSamples = 1000
	s.opts.MaxHeadBytes = 16384
	encoded, err := s.encodeHead(t.Context(), 1, testSamples(100))
	if err != nil {
		t.Fatal(err)
	}
	if _, err = s.decodeHead(t.Context(), headSnapshot{seriesID: 2, count: 100, start: testEpoch, end: testEpoch + 99, packed: encoded}); !errors.Is(err, ErrCorrupt) {
		t.Fatal("head rebound", err)
	}
}

func TestLatenessCannotBeBypassedToFitPackedCapacity(t *testing.T) {
	store, _ := openTestStore(t, Options{MaxHeadSamples: 32, Lateness: time.Hour})
	points := testSamples(33)
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points[:32]}}); err != nil {
		t.Fatal(err)
	}
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points[32:]}}); !errors.Is(err, ErrLimit) {
		t.Fatal("unsafe overflow accepted", err)
	}
	work, err := store.Maintain(t.Context())
	if err != nil || work.SealedBlocks != 0 {
		t.Fatal("capacity moved safe frontier", work, err)
	}
	assertSamples(t, readAll(t, store), points[:32])
}

// an opt-in incremental workload reports actual WAL bytes, not only the sealed file
func TestPackedHeadIncrementalWorkload(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1")
	}
	path := filepath.Join(t.TempDir(), "incremental.db")
	store, err := Open(t.Context(), path, Options{Retention: 400 * time.Millisecond, Lateness: 10 * time.Millisecond, MaxHeadSamples: 512})
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close(t.Context())
	now := testEpoch
	store.now = func() time.Time { return time.UnixMilli(now) }
	if err = store.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, pragmaErr := tx.ExecContext(t.Context(), `pragma wal_autocheckpoint=0`)
		return pragmaErr
	}); err != nil {
		t.Fatal(err)
	}
	random := rand.New(rand.NewPCG(9, 10))
	want := map[int64]float64{}
	start := time.Now()
	for i := range 1000 {
		now = testEpoch + int64(i)
		points := []Sample{{At: now, Value: float64(i % 11)}}
		if i > 5 {
			at := now - int64(random.IntN(5))
			points = append(points, Sample{At: at, Value: float64(i)})
		}
		for _, p := range points {
			want[p.At] = p.Value
		}
		if err = store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
			t.Fatal(err)
		}
		if _, err = store.Maintain(t.Context()); err != nil {
			t.Fatal(err)
		}
		actual := readAll(t, store)
		if len(actual) != min(i+1, 401) {
			t.Fatal("incremental sample count", i, len(actual))
		}
		for _, p := range actual {
			if math.Float64bits(p.Value) != math.Float64bits(want[p.At]) {
				t.Fatal("incremental bits")
			}
		}
	}
	info, err := os.Stat(path + "-wal")
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("1000 incremental ingest/retention/read rounds elapsed=%s WAL=%d bytes", time.Since(start), info.Size())
}

func TestPackedHeadGoldenOneSample(t *testing.T) {
	// raw frame: first=-0, one timestamp, no additional values
	s := encodingStore(t)
	s.opts.MaxHeadSamples = 10
	s.opts.MaxHeadBytes = 1000
	point := []Sample{{At: 10, Value: math.Copysign(0, -1)}}
	data, err := s.encodeHead(t.Context(), 7, point)
	if err != nil {
		t.Fatal(err)
	}
	const vector = "01011400010000000000000080080104000035fdc2448fd101eb"
	golden, err := hex.DecodeString(vector)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(golden, data) {
		t.Fatal("head version one changed")
	}
	back, err := s.decodeHead(t.Context(), headSnapshot{seriesID: 7, count: 1, start: 10, end: 10, packed: data})
	if err != nil {
		t.Fatal(err)
	}
	assertSamples(t, back, point)
}
