package records

import (
	"slices"
	"testing"
	"time"
)

// the example in the comment on routeToHeads
func TestARecordAMinuteBehindItsBatchIsLate(t *testing.T) {
	noon := time.Date(2026, 9, 25, 12, 0, 0, 0, time.UTC)
	batch := []Record{
		{At: noon.Add(10 * time.Second), Stream: "web", Name: "a"},
		{At: noon.Add(31 * time.Second), Stream: "web", Name: "b"},
		{At: noon.Add(-10*time.Minute + 2*time.Second), Stream: "web", Name: "c"},
		{At: noon.Add(45 * time.Second), Stream: "web", Name: "d"},
	}
	routed := routeToHeads(batch)
	if len(routed) != 2 || routed[0].late || !routed[1].late {
		t.Fatalf("routed %+v", routed)
	}
	names := func(records []Record) []string {
		var out []string
		for _, record := range records {
			out = append(out, record.Name)
		}
		return out
	}
	if !slices.Equal(names(routed[0].records), []string{"a", "b", "d"}) ||
		!slices.Equal(names(routed[1].records), []string{"c"}) {
		t.Fatalf("on time %v, late %v", names(routed[0].records), names(routed[1].records))
	}
}

// a head row holds no more than a block: a long batch becomes several rows
func TestAHeadRowHoldsABlocksWorth(t *testing.T) {
	routed := routeToHeads(frontendRecords(2*maxBlockRecords + 10))
	if len(routed) != 3 || len(routed[0].records) != maxBlockRecords || len(routed[2].records) != 10 {
		t.Fatalf("%d rows", len(routed))
	}
}
