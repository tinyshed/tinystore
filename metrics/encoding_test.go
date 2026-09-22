package metrics

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/binary"
	"errors"
	"hash/crc32"
	"math"
	"math/rand/v2"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/codec"
)

func encodingStore(t testing.TB) *Store {
	t.Helper()
	encoder, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	decoder, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	metadata, err := newMetadataCodec()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { encoder.Close(); decoder.Close(); metadata.close() })
	return &Store{encoder: encoder, decoder: decoder, metadata: metadata}
}

func encodedTestBlock(t testing.TB, s *Store, points []Sample) storedBlock {
	t.Helper()
	body, _, err := s.encodeValues(points, -2)
	if err != nil {
		t.Fatal(err)
	}
	b := storedBlock{format: 2, head: codec.Head{Start: points[0].At, End: points[len(points)-1].At, Count: len(points), First: points[0].Value}, clock: encodeClockValues(points), summary: summarize(points, Gauge)}
	b.body = sealValueBody(b, body)
	b.bodyBytes = len(b.body)
	return b
}

func TestEveryValueRepresentationPreservesBits(t *testing.T) {
	s := encodingStore(t)
	random := rand.New(rand.NewPCG(1, 2))
	for _, shape := range []string{"constant", "changes", "decimal", "grid", "arbitrary"} {
		points := make([]Sample, 240)
		at := int64(-10000)
		for i := range points {
			at += 10 + int64(random.IntN(4))*10
			value := float64(i % 37)
			switch shape {
			case "constant":
				value = math.Float64frombits(0x7ff800000000abcd)
			case "changes":
				value = float64(i / 80)
			case "decimal":
				value = float64(i%37) / 100
			case "grid":
				value = math.Nextafter(float64(i%37)/100, math.Inf(1))
			case "arbitrary":
				value = math.Float64frombits(random.Uint64())
			}
			points[i] = Sample{At: at, Value: value}
		}
		block := encodedTestBlock(t, s, points)
		back, err := s.decodeBlock(block)
		if err != nil {
			t.Fatalf("%s: %v", shape, err)
		}
		assertSamples(t, back, points)
		if shape == "constant" && len(block.body) != 0 {
			t.Fatal("constant has payload")
		}
		if shape == "changes" && block.body[0] != valuesChanges {
			t.Fatal("changes not selected")
		}
		if shape == "grid" {
			body, err := s.gridValues(points, 2)
			if err != nil || body == nil {
				t.Fatal("grid candidate", err)
			}
			block.body = sealValueBody(block, body)
			back, err = s.decodeBlock(block)
			if err != nil {
				t.Fatal(err)
			}
			assertSamples(t, back, points)
		}
	}
}

func TestResidualModesAreBoundedAndExact(t *testing.T) {
	s := encodingStore(t)
	random := rand.New(rand.NewPCG(7, 8))
	for count := range 240 {
		values := make([]int64, count)
		for i := range values {
			switch count % 4 {
			case 1:
				values[i] = int64(random.IntN(3)) - 1
			case 2:
				if i%27 == 0 {
					values[i] = int64(random.Uint64())
				}
			case 3:
				values[i] = int64(random.Uint64())
			}
		}
		encoded := s.metadata.encodeResiduals(values)
		decoded, err := s.metadata.decodeResiduals(encoded, count)
		if err != nil {
			t.Fatal(err)
		}
		for i := range values {
			if values[i] != decoded[i] {
				t.Fatal("residual changed")
			}
		}
	}
}

func TestClockSharingAndLastOwnerRetention(t *testing.T) {
	store, _ := openTestStore(t, Options{Retention: time.Second})
	first := testSeries()
	second := testSeries()
	second.Labels[0].Value = "two"
	points := testSamples(800)
	for i := range points {
		points[i].Value = float64(i / 200)
	}
	if err := store.Ingest(t.Context(), []Batch{{Series: first, Samples: points}, {Series: second, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	check := func(wantClocks, wantRefs int) {
		t.Helper()
		err := store.file.View(t.Context(), func(tx *sql.Tx) error {
			var clocks, refs int
			err := tx.QueryRowContext(t.Context(), `select count(*),coalesce(sum(refs),0) from clocks`).Scan(&clocks, &refs)
			if clocks != wantClocks || refs != wantRefs {
				t.Errorf("clock ownership %d/%d want %d/%d", clocks, refs, wantClocks, wantRefs)
			}
			return err
		})
		if err != nil {
			t.Fatal(err)
		}
	}
	check(1, 2)
	result, err := store.Read(t.Context(), Range{Matchers: []Label{{Name: "__name__", Value: "cpu"}}, From: testEpoch, To: testEpoch + 800})
	if err != nil || len(result) != 2 {
		t.Fatal("shared read", err)
	}
	for _, series := range result {
		assertSamples(t, series.Samples, points)
	}
	if _, err = store.expireSeries(t.Context(), 1, testEpoch+900); err != nil {
		t.Fatal(err)
	}
	check(1, 1)
	result, err = store.Read(t.Context(), Range{Matchers: second.Labels, From: testEpoch, To: testEpoch + 800})
	if err != nil {
		t.Fatal(err)
	}
	assertSamples(t, result[0].Samples, points)
	if _, err = store.expireSeries(t.Context(), 2, testEpoch+900); err != nil {
		t.Fatal(err)
	}
	check(0, 0)
}

func TestBlockSpanIsBoundedWithoutDroppingThePrefix(t *testing.T) {
	store, _ := openTestStore(t, Options{MaxBlockSpan: 10 * time.Millisecond})
	points := testSamples(500)
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
		group, _, err := store.firstGroup(t.Context(), tx, 1)
		if err != nil {
			return err
		}
		for _, b := range group.blocks {
			if b.head.End-b.head.Start > 10 {
				t.Fatal("block span")
			}
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	assertSamples(t, readAll(t, store), points)
}

func FuzzNewFormats(f *testing.F) {
	s := encodingStore(f)
	points := []Sample{{At: 10, Value: 1}, {At: 20, Value: 2}, {At: 40, Value: 2}}
	block := encodedTestBlock(f, s, points)
	g := blockGroup{format: 2, seriesID: 1, start: 10, end: 40, live: 1, clockID: 1, blocks: []storedBlock{block}}
	if block.bodyBytes > inlineBytes {
		g.allocation = 1
		g.firstPayload = 1
	}
	dir, err := s.writeDirectory(g)
	if err != nil {
		f.Fatal(err)
	}
	f.Add(byte(0), encodeClockGroup(g))
	f.Add(byte(1), block.body)
	f.Add(byte(2), dir)
	f.Add(byte(3), []byte{residualSparse, 1, 1})
	g.format, g.firstPayload, g.allocation = 3, 0, 1
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
			_, _ = s.readDirectory(1, 10, 40, 1, data, []storedBlock{block})
		case 3:
			_, _ = s.metadata.decodeResiduals(data, 2)
		}
	})
}

func TestClockCorruptionIsRefused(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: testSamples(500)}}); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := store.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), `update clocks set body=zeroblob(20)`)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	_, err := store.Read(context.Background(), Range{Matchers: testSeries().Labels, From: 0, To: math.MaxInt64})
	if !errors.Is(err, ErrCorrupt) {
		t.Fatal("clock corruption", err)
	}
}
