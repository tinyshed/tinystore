package metrics

import (
	"bytes"
	"database/sql"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"hash/crc32"
	"testing"

	"github.com/tinyshed/tinystore/codec"
)

// a directory is the bytes of its vectors: a block kept inline, its summary
// predicted and its sums exact, and one whose body is the payload its id names
func TestDirectoriesWrittenBeforeStillRead(t *testing.T) {
	s := encodingStore(t)
	clock, err := hex.DecodeString("010100ef01f00100b9a118b1")
	if err != nil {
		t.Fatal(err)
	}
	clocks, err := decodeClockGroup(clock)
	if err != nil {
		t.Fatal(err)
	}
	for _, c := range []struct {
		name     string
		external bool
		vector   string
	}{
		{"inline", false, "04010100000000000000010000000000000000d0027f0002ee10013b000096af3baf"},
		{"external", true, "04010100000001000000010000000000000000d0027f0002ee10013b0014f0a204d6e62401"},
	} {
		group := constantGroup(clocks, c.external)
		written, err := s.writeDirectory(group)
		if err != nil || hex.EncodeToString(written) != c.vector {
			t.Errorf("%s: written as %x, %v", c.name, written, err)
		}
		vector, err := hex.DecodeString(c.vector)
		if err != nil {
			t.Fatal(err)
		}
		read, err := s.readDirectory(7, groupRow{start: 0, end: 239, clockID: 1, data: vector}, clocks)
		if err != nil {
			t.Fatalf("%s: %v", c.name, err)
		}
		want, got := group.blocks[0], read.blocks[0]
		if got.payload != want.payload || got.bodyBytes != want.bodyBytes || got.summary.sum != 10080 ||
			!bytes.Equal(got.summary.exactSum, want.summary.exactSum) {
			t.Errorf("%s: read back as %+v", c.name, got)
		}
		if c.external {
			continue
		}
		points, err := s.decodeBlock(got)
		if err != nil || len(points) != 240 || points[239] != (Sample{At: 239, Value: 42}) {
			t.Errorf("%s: decoded %d samples, %v", c.name, len(points), err)
		}
	}
}

// constantGroup is one block of 240 samples of 42, a millisecond apart, kept
// inline, or as the payload 70000 when external
func constantGroup(clocks []storedBlock, external bool) blockGroup {
	points := make([]Sample, 240)
	for i := range points {
		points[i] = Sample{At: int64(i), Value: 42}
	}
	block := clocks[0]
	block.head = codec.Head{Start: 0, End: 239, Count: 240, First: 42}
	block.summary = exactSummarize(points, Gauge)
	group := blockGroup{seriesID: 7, start: 0, end: 239, clockID: 1, live: 1}
	if external {
		block.bodyBytes, block.payload = 20, 70000
		group.allocation = 1
	}
	group.blocks = []storedBlock{block}
	return group
}

func TestDirectoryCorruptionAndRebindingAreRefused(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	points := testSamples(500)
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	var directory []byte
	var start, end, clockID int64
	if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select start_ts,end_ts,directory,clock_id from groups where series_id=1`).Scan(&start, &end, &directory, &clockID)
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.readDirectory(2, groupRow{start: start, end: end, clockID: clockID, data: directory}, nil); !errors.Is(err, ErrCorrupt) {
		t.Fatalf("rebound directory: %v", err)
	}
	directory[len(directory)/2] ^= 1
	if err := store.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), `update groups set directory=? where series_id=1`, directory)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if result, err := store.Read(t.Context(), Range{Name: testSeries().Name, Match: testSeries().Labels, From: testEpoch, To: testEpoch + 500}); !errors.Is(err, ErrCorrupt) || result != nil {
		t.Fatalf("corruption: %v %#v", err, result)
	}
}

func FuzzNewFormats(f *testing.F) {
	s := encodingStore(f)
	points := []Sample{{At: 10, Value: 1}, {At: 20, Value: 2}, {At: 40, Value: 2}}
	block := encodedTestBlock(f, s, points)
	g := blockGroup{seriesID: 1, start: 10, end: 40, live: 1, clockID: 1, blocks: []storedBlock{block}}
	if block.bodyBytes > inlineBytes {
		g.allocation, g.blocks[0].payload = 1, 1
	}
	dir, err := s.writeDirectory(g)
	if err != nil {
		f.Fatal(err)
	}
	f.Add(byte(0), encodeClockGroup(g))
	f.Add(byte(1), block.body)
	f.Add(byte(2), dir)
	f.Add(byte(3), []byte{residualSparse, 1, 1})
	g.allocation = 1
	g.blocks[0].bodyBytes, g.blocks[0].payload = 20, 70000
	dir, err = s.writeDirectory(g)
	if err != nil {
		f.Fatal(err)
	}
	f.Add(byte(2), dir)
	f.Fuzz(func(t *testing.T, kind byte, data []byte) {
		if len(data) > maxDirectoryBytes {
			return
		}
		data = bytes.Clone(data)
		switch kind % 4 {
		case 0:
			if len(data) >= 4 {
				binary.LittleEndian.PutUint32(data[len(data)-4:], crc32.ChecksumIEEE(data[:len(data)-4]))
			}
			_, _ = decodeClockGroup(data)
		case 1:
			b := block
			b.body = data
			if len(data) >= 4 {
				binary.LittleEndian.PutUint32(data[len(data)-4:], valueChecksum(b, data[:len(data)-4]))
			}
			out, err := s.decodeBlock(b)
			if err == nil && len(out) != len(points) {
				t.Fatal("unbounded values")
			}
		case 2:
			if len(data) >= 4 {
				binary.LittleEndian.PutUint32(data[len(data)-4:], g.checksum(data[:len(data)-4]))
			}
			_, _ = s.readDirectory(1, groupRow{start: 10, end: 40, clockID: 1, data: data}, []storedBlock{block})
		case 3:
			_, _ = s.metadata.decodeResiduals(data, 2)
		}
	})
}
