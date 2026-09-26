package records

import (
	"database/sql"
	"math"
	"slices"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// the examples in the comment on routeToHeads
func TestARecordTenSecondsBehindItsStreamsNewestIsLate(t *testing.T) {
	noon := time.Date(2026, 9, 25, 12, 0, 0, 0, time.UTC)
	waiting := waitingTimes{newest: map[string]int64{"web": noon.Add(40 * time.Second).UnixNano()}}
	batch := []Record{
		{At: noon.Add(35 * time.Second), Stream: "web", Name: "a"},
		{At: noon.Add(41 * time.Second), Stream: "web", Name: "b"},
		{At: noon.Add(-10*time.Minute + 2*time.Second), Stream: "web", Name: "c"},
		{At: noon.Add(45 * time.Second), Stream: "web", Name: "d"},
	}
	clock := noon.Add(time.Minute).UnixNano()
	routed := routeToHeads(batch, &waiting, clock)
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
	inOrder := []Record{
		{At: noon.Add(-90 * time.Second), Stream: "web", Name: "f"},
		{At: noon.Add(-50 * time.Second), Stream: "web", Name: "g"},
		{At: noon.Add(45 * time.Second), Stream: "web", Name: "h"},
	}
	if routed = routeToHeads(inOrder, &waitingTimes{newest: map[string]int64{}}, clock); len(routed) != 1 || routed[0].late {
		t.Fatalf("a batch in time order over two minutes: %+v", routed)
	}
	alone := routeToHeads([]Record{{At: noon.Add(20 * time.Second), Stream: "web", Name: "e"}}, &waiting, clock)
	if len(alone) != 1 || !alone[0].late {
		t.Fatalf("a record appended alone, 20 s behind what waits: %+v", alone)
	}
	ahead := waitingTimes{newest: map[string]int64{"web": noon.Add(5 * time.Minute).UnixNano()}}
	alone = routeToHeads([]Record{{At: noon.Add(-5 * time.Second), Stream: "web", Name: "e"}}, &ahead, noon.UnixNano())
	if len(alone) != 1 || alone[0].late {
		t.Fatalf("5 s behind the clock, behind a producer five minutes ahead of it: %+v", alone)
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
	if late := s.recordsInLateHeads(t); late != 1 {
		t.Fatalf("%d records in the late head, want 1", late)
	}
	s.clock.advance(time.Hour)
	s.maintain(t)
	if _, remembered := s.waiting.newest["frontend"]; remembered {
		t.Fatal("a head that sealed empty still places its stream")
	}
}

// a producer whose clock runs ahead of the store's, within its skew, does not
// send the records of the producers beside it to the late head
func TestAProducerAheadOfTheStoreDoesNotMakeItsNeighboursLate(t *testing.T) {
	s := openRecords(t)
	s.append(t, Record{At: testNow.Add(5 * time.Minute), Stream: "web", Name: "ahead"})
	s.append(t, Record{At: testNow.Add(-5 * time.Second), Stream: "web", Name: "on time"})
	if late := s.recordsInLateHeads(t); late != 0 {
		t.Fatalf("%d records in the late head, want none", late)
	}
}

// a store reopened with records waiting in a head still places a record
// appended alone against them
func TestAReopenedStoreKnowsWhatItsHeadsHold(t *testing.T) {
	dir := t.TempDir()
	s := openTestStore(t, dir, Options{}, tinystore.Options{})
	records := frontendRecords(3)
	s.append(t, records...)
	if err := s.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}

	reopened := openTestStore(t, dir, Options{}, tinystore.Options{})
	late := records[2]
	late.At = late.At.Add(-10 * time.Minute)
	reopened.append(t, late)
	if got := reopened.recordsInLateHeads(t); got != 1 {
		t.Fatalf("%d records in the late head after reopening, want 1", got)
	}
}

func (s *testStore) recordsInLateHeads(t *testing.T) int {
	t.Helper()
	var late int
	err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select coalesce(sum(count), 0) from head_state where late = 1`).
			Scan(&late)
	})
	if err != nil {
		t.Fatal(err)
	}
	return late
}

// a head row holds no more than a block: a long batch becomes several rows
func TestAHeadRowHoldsABlocksWorth(t *testing.T) {
	routed := routeToHeads(frontendRecords(2*maxBlockRecords+10), &waitingTimes{newest: map[string]int64{}}, math.MaxInt64)
	if len(routed) != 3 || len(routed[0].records) != maxBlockRecords || len(routed[2].records) != 10 {
		t.Fatalf("%d rows", len(routed))
	}
}
