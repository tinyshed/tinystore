package metrics

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
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
	path := filepath.Join(t.TempDir(), fileName)
	store, err := openAt(t, path, Options{Retention: 400 * time.Millisecond, Lateness: 10 * time.Millisecond, MaxHeadSamples: 512})
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

func TestUnchangedHeadChunksKeepTheirEncodedBytes(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	old := testSamples(480)
	packed, err := s.encodeHead(t.Context(), 1, old)
	if err != nil {
		t.Fatal(err)
	}
	chunks, err := s.parseHead(snapshotOf(1, old, packed))
	if err != nil {
		t.Fatal(err)
	}
	for _, test := range []struct {
		name     string
		incoming []Sample
		want     int
	}{
		{name: "append", incoming: []Sample{{At: testEpoch + 480, Value: math.Float64frombits(0x7ff8000000004321)}}, want: 480},
		{name: "late replacement", incoming: []Sample{{At: testEpoch + 300, Value: math.Float64frombits(0x7ff8000000004321)}}, want: 240},
	} {
		t.Run(test.name, func(t *testing.T) {
			merged, err := mergeHead(old, test.incoming, s.opts.MaxHeadSamples)
			if err != nil {
				t.Fatal(err)
			}
			kept := reusableChunks(chunks, test.incoming[0].At)
			if countChunkSamples(kept) != test.want {
				t.Fatalf("reusable prefix: %d", countChunkSamples(kept))
			}
			updated, err := s.encodeHeadAfter(t.Context(), 1, kept, merged[test.want:])
			if err != nil {
				t.Fatal(err)
			}
			after, err := s.parseHead(snapshotOf(1, merged, updated))
			if err != nil {
				t.Fatal(err)
			}
			if !bytes.Equal(storedBytes(reusableChunks(after, test.incoming[0].At)), storedBytes(kept)) {
				t.Fatal("unchanged chunk bytes moved")
			}
			decoded, err := s.decodeHead(t.Context(), snapshotOf(1, merged, updated))
			if err != nil {
				t.Fatal(err)
			}
			for i := range merged {
				if decoded[i].At != merged[i].At || math.Float64bits(decoded[i].Value) != math.Float64bits(merged[i].Value) {
					t.Fatalf("sample %d changed", i)
				}
			}
		})
	}
}

func snapshotOf(id int64, points []Sample, packed []byte) headSnapshot {
	return headSnapshot{seriesID: id, count: len(points), start: points[0].At, end: points[len(points)-1].At, packed: packed}
}

func storedBytes(chunks []headChunk) []byte {
	var out []byte
	for _, chunk := range chunks {
		out = append(out, chunk.stored...)
	}
	return out
}

func TestNarrowPackedHeadChargesSelectedChunksAndChecksWholeChecksum(t *testing.T) {
	s, _ := openTestStore(t, Options{MaxHeadSamples: 512})
	points := testSamples(481)
	if err := s.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
		t.Fatal(err)
	}
	last := points[len(points)-1]
	request := Range{Matchers: testSeries().Labels, From: last.At, To: last.At + 1, Limits: Limits{DecodedSamples: 1, OutputSamples: 1}}
	read, err := s.Read(t.Context(), request)
	if err != nil || len(read) != 1 || len(read[0].Samples) != 1 || math.Float64bits(read[0].Samples[0].Value) != math.Float64bits(last.Value) {
		t.Fatalf("narrow last chunk: %+v, %v", read, err)
	}
	firstChunk := Range{Matchers: testSeries().Labels, From: points[1].At, To: points[1].At + 1, Limits: Limits{DecodedSamples: 239}}
	if read, err := s.Read(t.Context(), firstChunk); !errors.Is(err, ErrLimit) || read != nil {
		t.Fatalf("undersized selected chunk budget: %+v, %v", read, err)
	}
	firstChunk.Limits.DecodedSamples = 240
	if read, err := s.Read(t.Context(), firstChunk); err != nil || len(read) != 1 || len(read[0].Samples) != 1 {
		t.Fatalf("selected first chunk: %+v, %v", read, err)
	}
	if err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
		var tail []byte
		if err := tx.QueryRowContext(t.Context(), `select tail from series_state where series_id=1`).Scan(&tail); err != nil {
			return err
		}
		tail[6] ^= 0xff
		_, err := tx.ExecContext(t.Context(), `update series_state set tail=? where series_id=1`, tail)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if read, err := s.Read(t.Context(), request); !errors.Is(err, ErrCorrupt) || read != nil {
		t.Fatalf("unselected prefix corruption: %+v, %v", read, err)
	}
}

func TestBatchedNarrowHeadsChargeSelectedChunks(t *testing.T) {
	s, _ := openTestStore(t, Options{MaxHeadSamples: 512})
	points := testSamples(481)
	batches := make([]Batch, 20)
	for i := range batches {
		batches[i] = Batch{Series: Series{Labels: []Label{{Name: "__name__", Value: "cpu"}, {Name: "host", Value: fmt.Sprint(i)}}}, Samples: points}
	}
	if err := s.Ingest(t.Context(), batches); err != nil {
		t.Fatal(err)
	}
	last := points[len(points)-1]
	request := Range{Matchers: []Label{{Name: "__name__", Value: "cpu"}}, From: last.At, To: last.At + 1, Limits: Limits{DecodedSamples: 20, OutputSamples: 20}}
	read, err := s.Read(t.Context(), request)
	if err != nil || len(read) != 20 {
		t.Fatalf("batched narrow heads: %d series, %v", len(read), err)
	}
	for _, series := range read {
		if len(series.Samples) != 1 || math.Float64bits(series.Samples[0].Value) != math.Float64bits(last.Value) {
			t.Fatalf("batched narrow value changed: %+v", series)
		}
	}
}
