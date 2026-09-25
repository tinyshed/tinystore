package records

import (
	"encoding/binary"
	"errors"
	"hash/crc32"
	"slices"
	"testing"

	"github.com/tinyshed/tinystore"
)

// every stored byte is covered by a checksum, so a changed or missing one is
// refused rather than decoded into different records
func TestAChangedOrMissingByteIsRefused(t *testing.T) {
	e, d := testCoders(t)
	segment := e.encodeSegment("backend", backendRecords(200))
	s, err := d.parseSchema(segment.row)
	if err != nil {
		t.Fatal(err)
	}
	body := segment.blocks[0].body
	for i := range max(len(segment.row), len(body)) {
		if i < len(segment.row) {
			if _, err = d.parseSchema(flipByte(segment.row, i)); !errors.Is(err, tinystore.ErrCorrupt) {
				t.Fatalf("segment row with byte %d changed: %v", i, err)
			}
		}
		if i < len(body) {
			if _, err = d.openBlock(s, flipByte(body, i)); !errors.Is(err, tinystore.ErrCorrupt) {
				t.Fatalf("block with byte %d changed: %v", i, err)
			}
			if _, err = d.openBlock(s, body[:i]); !errors.Is(err, tinystore.ErrCorrupt) {
				t.Fatalf("block cut at byte %d: %v", i, err)
			}
		}
	}
}

func flipByte(data []byte, at int) []byte {
	changed := slices.Clone(data)
	changed[at] ^= 0x20
	return changed
}

// withChecksum rewrites the trailing checksum, so that a fuzzed row reaches
// the parser behind it instead of stopping at the checksum
func withChecksum(data []byte) []byte {
	if len(data) < 5 {
		return data
	}
	fixed := slices.Clone(data)
	end := len(fixed) - 4
	binary.LittleEndian.PutUint32(fixed[end:], crc32.ChecksumIEEE(fixed[:end]))
	return fixed
}

func FuzzBlock(f *testing.F) {
	e, _ := testCoders(f)
	schemas := map[string][]byte{}
	for _, records := range [][]Record{frontendRecords(300), backendRecords(300), edgeRecords()} {
		segment := e.encodeSegment(records[0].Stream, slices.Clone(records))
		schemas[records[0].Stream] = segment.row
		f.Add(records[0].Stream, segment.blocks[0].body)
	}
	_, d := testCoders(f)
	f.Fuzz(func(t *testing.T, stream string, body []byte) {
		row, ok := schemas[stream]
		if !ok {
			return
		}
		s, err := d.parseSchema(row)
		if err != nil {
			t.Fatal(err)
		}
		block, err := d.openBlock(s, withChecksum(body))
		if err != nil {
			return
		}
		_, _ = d.records(block, nil)
	})
}

// the table in the comment on levelBit
func TestLevelsShareABitByFours(t *testing.T) {
	for level, bit := range map[int64]int64{
		-8: 0, -4: 0, -1: 0, 0: 1, 3: 1, 4: 2, 8: 3, 11: 3, 12: 4, 16: 5, 20: 6, 23: 6, 24: 7, 1 << 30: 7,
	} {
		if got := levelBit(level); got != 1<<bit {
			t.Errorf("level %d sets %b, want bit %d", level, got, bit)
		}
	}
	if mask := levelsFrom(8); mask != 0b11111000 {
		t.Errorf("errors and above: %b", mask)
	}
}
