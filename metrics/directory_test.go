package metrics

import (
	"bytes"
	"database/sql"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"hash/crc32"
	"testing"
)

func TestVersionTwoDirectoryAndClockRemainReadable(t *testing.T) {
	const clockVector = "010100ef01f00100b9a118b1"
	const directoryVector = "020101000000000000000000000000000000010000000000000000d0023f0000325ad605"
	s := encodingStore(t)
	clock, err := hex.DecodeString(clockVector)
	if err != nil {
		t.Fatal(err)
	}
	directory, err := hex.DecodeString(directoryVector)
	if err != nil {
		t.Fatal(err)
	}
	blocks, err := decodeClockGroup(clock)
	if err != nil {
		t.Fatal(err)
	}
	group, err := s.readDirectory(7, groupRow{start: 0, end: 239, clockID: 1, data: directory}, blocks)
	if err != nil {
		t.Fatal(err)
	}
	if len(group.blocks) != 1 || len(group.blocks[0].body) != 0 || group.blocks[0].summary.sum != 10080 {
		t.Fatal("constant summary changed")
	}
	encoded, err := s.writeDirectory(group)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(encoded, directory) {
		t.Fatal("version two changed")
	}
	points, err := s.decodeBlock(group.blocks[0])
	if err != nil {
		t.Fatal(err)
	}
	if len(points) != 240 {
		t.Fatal("count changed")
	}
	for i, point := range points {
		if point.At != int64(i) || point.Value != 42 {
			t.Fatal("sample changed")
		}
	}
}

func TestVersionThreeDirectoryKeepsPayloadAddresses(t *testing.T) {
	const clockVector = "010100ef01f00100b9a118b1"
	const directoryVector = "030101000000010000000000000000000000010000000000000000d0023f0014f0a204504d30aa"
	s := encodingStore(t)
	clock, err := hex.DecodeString(clockVector)
	if err != nil {
		t.Fatal(err)
	}
	directory, err := hex.DecodeString(directoryVector)
	if err != nil {
		t.Fatal(err)
	}
	blocks, err := decodeClockGroup(clock)
	if err != nil {
		t.Fatal(err)
	}
	group, err := s.readDirectory(7, groupRow{start: 0, end: 239, clockID: 1, data: directory}, blocks)
	if err != nil {
		t.Fatal(err)
	}
	if group.format != 3 || group.payloadID(0) != 70000 || group.blocks[0].bodyBytes != 20 {
		t.Fatal("explicit payload address changed")
	}
	encoded, err := s.writeDirectory(group)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(encoded, directory) {
		t.Fatal("version three changed")
	}
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
	if result, err := store.Read(t.Context(), Range{Matchers: testSeries().Labels, From: testEpoch, To: testEpoch + 500}); !errors.Is(err, ErrCorrupt) || result != nil {
		t.Fatalf("corruption: %v %#v", err, result)
	}
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
			_, _ = s.readDirectory(1, groupRow{start: 10, end: 40, clockID: 1, data: data}, []storedBlock{block})
		case 3:
			_, _ = s.metadata.decodeResiduals(data, 2)
		}
	})
}
