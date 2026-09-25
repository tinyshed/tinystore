package records

import (
	"database/sql"
	"slices"
	"testing"
	"time"
)

// the examples in the comment on routeToHeads
func TestARecordAMinuteBehindItsStreamsNewestIsLate(t *testing.T) {
	noon := time.Date(2026, 9, 25, 12, 0, 0, 0, time.UTC)
	waiting := waitingTimes{newest: map[string]int64{"web": noon.Add(40 * time.Second).UnixNano()}}
	batch := []Record{
		{At: noon.Add(10 * time.Second), Stream: "web", Name: "a"},
		{At: noon.Add(31 * time.Second), Stream: "web", Name: "b"},
		{At: noon.Add(-10*time.Minute + 2*time.Second), Stream: "web", Name: "c"},
		{At: noon.Add(45 * time.Second), Stream: "web", Name: "d"},
	}
	routed := routeToHeads(batch, &waiting)
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
	alone := routeToHeads([]Record{{At: noon.Add(-30 * time.Second), Stream: "web", Name: "e"}}, &waiting)
	if len(alone) != 1 || !alone[0].late {
		t.Fatalf("a record appended alone, 70 s behind what waits: %+v", alone)
	}
}

// a record appended alone is late when it lags the records its stream has
// waiting; once its head seals empty, the stream starts again from its batches
func TestARecordAppendedAloneCanBeLate(t *testing.T) {
	s := openRecords(t)
	records := frontendRecords(3)
	for _, record := range records {
		s.append(t, record)
	}
	late := records[2]
	late.At = late.At.Add(-10 * time.Minute)
	s.append(t, late)
	var inLateHead int
	err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select coalesce(sum(count), 0) from head_state where late = 1`).
			Scan(&inLateHead)
	})
	if err != nil || inLateHead != 1 {
		t.Fatalf("%d records in the late head, want 1: %v", inLateHead, err)
	}
	s.clock.advance(time.Hour)
	s.maintain(t)
	if _, remembered := s.waiting.newest["frontend"]; remembered {
		t.Fatal("a head that sealed empty still places its stream")
	}
}

// a head row holds no more than a block: a long batch becomes several rows
func TestAHeadRowHoldsABlocksWorth(t *testing.T) {
	routed := routeToHeads(frontendRecords(2*maxBlockRecords+10), &waitingTimes{newest: map[string]int64{}})
	if len(routed) != 3 || len(routed[0].records) != maxBlockRecords || len(routed[2].records) != 10 {
		t.Fatalf("%d rows", len(routed))
	}
}
