package records

import (
	"slices"
	"testing"
	"time"
)

func TestShapesPastTheirBoundKeepAttributesWhole(t *testing.T) {
	e, d := testCoders(t)
	records := make([]Record, 300)
	for i := range records {
		records[i] = Record{
			At: time.Unix(0, fixtureBase+int64(i)).UTC(), Stream: "webhook", Name: "external",
			Attrs: []Field{{Key: "field_" + string(rune('a'+i%26)) + string(rune('a'+i/26)), Value: "1"}, {"id", `"x"`}},
		}
	}
	segment := e.encodeSegment("webhook", slices.Clone(records))
	s, err := d.parseSchema(segment.row)
	if err != nil {
		t.Fatal(err)
	}
	raw := 0
	for _, shape := range s.shapes {
		if shape.presence&rawAttrs != 0 {
			raw++
		}
	}
	if len(s.shapes) != maxTypedShapes+1 || raw != 1 {
		t.Fatalf("%d shapes, %d of them serialized; want %d typed and one serialized", len(s.shapes), raw, maxTypedShapes)
	}
	sameRecords(t, records, decodeSegment(t, d, segment))
}

func FuzzSegment(f *testing.F) {
	e, d := testCoders(f)
	for _, records := range [][]Record{frontendRecords(300), backendRecords(300), edgeRecords()} {
		f.Add(e.encodeSegment(records[0].Stream, slices.Clone(records)).row)
	}
	f.Fuzz(func(t *testing.T, row []byte) {
		_, _ = d.parseSchema(withChecksum(row))
	})
}
