package records

import (
	"encoding/hex"
	"errors"
	"slices"
	"testing"

	"github.com/tinyshed/tinystore"
)

func testHeadStore(t testing.TB) *Store {
	t.Helper()
	coders, err := newBlobCoders()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = coders.close() })
	return &Store{blobs: coders.segments, heads: coders.heads, unpack: coders.unpack}
}

// a head row keeps its records in arrival order, whatever their times
func TestHeadRowsRoundTripInArrivalOrder(t *testing.T) {
	s := testHeadStore(t)
	d := newDecoder(s.unpack)
	for name, records := range map[string][]Record{
		"frontend": frontendRecords(maxBlockRecords), "backend": backendRecords(300), "edge": edgeRecords(),
	} {
		row := s.encodeHeadRow(headBatch{stream: records[0].Stream, records: records})
		decoded, err := d.parseHeadRow(records[0].Stream, row.body)
		if err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		sameRecords(t, records, decoded)
	}
}

func TestAChangedHeadRowIsRefused(t *testing.T) {
	s := testHeadStore(t)
	d := newDecoder(s.unpack)
	row := s.encodeHeadRow(headBatch{stream: "backend", records: backendRecords(20)})
	for i := range row.body {
		if _, err := d.parseHeadRow("backend", flipByte(row.body, i)); !errors.Is(err, tinystore.ErrCorrupt) {
			t.Fatalf("a head row with byte %d changed: %v", i, err)
		}
		if _, err := d.parseHeadRow("backend", row.body[:i]); !errors.Is(err, tinystore.ErrCorrupt) {
			t.Fatalf("a head row cut at byte %d: %v", i, err)
		}
	}
}

// the first head row format still reads, and is still what the engine writes
func TestHeadRowsWrittenBeforeStillRead(t *testing.T) {
	s := testHeadStore(t)
	records := slices.Concat(backendRecords(20), edgeRecords())
	for i := range records {
		records[i].Stream = "golden"
	}
	written := hex.EncodeToString(s.encodeHeadRow(headBatch{stream: "golden", records: records}).body)
	stored := goldenRows(t, "head-v1.json", written)
	decoded, err := newDecoder(s.unpack).parseHeadRow("golden", unhex(t, stored))
	if err != nil {
		t.Fatal(err)
	}
	sameRecords(t, records, decoded)
	if stored != written {
		t.Error("the encoder no longer writes the stored head row")
	}
}

func FuzzHeadRow(f *testing.F) {
	s := testHeadStore(f)
	for _, records := range [][]Record{frontendRecords(30), backendRecords(30), edgeRecords()} {
		f.Add(s.encodeHeadRow(headBatch{stream: "fuzz", records: records}).body)
	}
	d := newDecoder(s.unpack)
	f.Fuzz(func(t *testing.T, body []byte) {
		_, _ = d.parseHeadRow("fuzz", withChecksum(body))
	})
}
