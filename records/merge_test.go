package records

import (
	"database/sql"
	"errors"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// a merged segment's places are found through their holder's index, not by
// walking every segment sealed after it
func TestPlacesAreFoundThroughTheirHolder(t *testing.T) {
	s := openRecords(t)
	for statement, arguments := range map[string][]any{movePlacesInto: {1, 2, 3}, deleteMergedPlaces: {1}} {
		if plan := s.queryPlan(t, statement, arguments...); !strings.Contains(plan, "segments_by_holder (holder=?)") {
			t.Errorf("%s: plan %q", statement, plan)
		}
	}
}

// the examples in the comment on sizeClass
func TestASizeClassIsThePowerOfFourItsRecordsStayUnder(t *testing.T) {
	for records, want := range map[int]int{1: 1, 3: 1, 4: 2, 15: 2, 16: 3, 63: 3, 143: 4, 593: 5} {
		if got := sizeClass(records); got != want {
			t.Errorf("%d records: size class %d, want %d", records, got, want)
		}
	}
}

// the example in the comment on merging: four of a power of four make one of the next
func TestSmallSegmentsMergeFourOfASize(t *testing.T) {
	for _, sizes := range [][]int{{30, 40, 35, 38}, {143, 150, 160, 140}} {
		var candidates []mergeable
		for i, held := range sizes {
			at := int64(i) * int64(time.Hour)
			candidates = append(candidates, mergeable{id: int64(i + 1), first: at, last: at + 1, held: held})
		}
		if run := pickMerge(candidates); len(run) != mergeFanIn {
			t.Errorf("%v: merged %d", sizes, len(run))
		}
	}
	mixed := []mergeable{
		{id: 1, held: 30},
		{id: 2, first: 2, last: 3, held: 40},
		{id: 3, first: 4, last: 5, held: 35},
		{id: 4, first: 6, last: 7, held: 70},
	}
	if run := pickMerge(mixed); run != nil {
		t.Errorf("three of a size and one of the next merged: %+v", run)
	}
}

// a segment that overlaps the one before it in time is left out of a merge,
// so that the records of a run, one member after another, are in time order
func TestAMergeLeavesOverlappingSegmentsOut(t *testing.T) {
	segments := []mergeable{
		{id: 1, first: 0, last: 10, held: 20},
		{id: 2, first: 5, last: 15, held: 20},
		{id: 3, first: 20, last: 30, held: 20},
		{id: 4, first: 30, last: 50, held: 20},
		{id: 5, first: 60, last: 70, held: 20},
	}
	run := pickMerge(segments)
	var ids []int64
	for _, member := range run {
		ids = append(ids, member.id)
	}
	if !slices.Equal(ids, []int64{1, 3, 4, 5}) {
		t.Fatalf("merged %v, want 1 3 4 5", ids)
	}
	if run = pickMerge(segments[:4]); run != nil {
		t.Fatalf("merged three that do not overlap: %+v", run)
	}
}

// hourly appends a stream's records for one hour, and seals them an hour later
func (s *testStore) hourly(t *testing.T, stream string, count int) []Record {
	t.Helper()
	records := make([]Record, count)
	for i := range records {
		at := s.clock.Now().Add(-time.Hour).Add(time.Duration(i) * time.Second)
		records[i] = Record{At: at, Stream: stream, Name: "tick", Attrs: []Field{Int("i", int64(i))}}
	}
	s.append(t, records...)
	return records
}

// four hours of a quiet stream make one segment of four hours' records, which
// reads and follows as the four did; the example in docs/records.md
func TestFourQuietHoursMergeIntoOneSegment(t *testing.T) {
	s := openRecords(t)
	var all []Record
	for hour, count := range []int{30, 40, 35, 38} {
		all = append(all, s.hourly(t, "quiet", count)...)
		s.clock.advance(time.Hour)
		work := s.maintain(t)
		if merged := work.MergedSegments; (hour < 3 && merged != 0) || (hour == 3 && merged != 3) {
			t.Fatalf("hour %d merged %d segments", hour, merged)
		}
	}
	if c := s.counts(t); c.segments != 4 || c.blocks != 1 {
		t.Fatalf("after the merge: %+v", c)
	}
	if starts := s.ids(t, `select start from segments order by id`); !slices.Equal(starts, []int64{0, 30, 70, 105}) {
		t.Fatalf("places start at %v", starts)
	}
	sameRecords(t, sortedByTime(all), s.readAll(t, Query{}))
	followed, next, expired := s.followAll(t, Cursor{}, 7)
	sameRecords(t, sortedByTime(all), followed)
	if next != (Cursor{Segment: 5}) || expired != 0 || s.Stats().MergedSegments != 3 {
		t.Fatalf("followed to %+v, %d expired, stats %+v", next, expired, s.Stats())
	}
}

// A follower's cursor names a place in the order segments were sealed. A merge
// moves the records of places, and each follower goes on from where it was, the
// middle of a place included, and finds every record once.
func TestAFollowerKeepsItsPlaceAcrossMerges(t *testing.T) {
	s := openRecords(t)
	for range 3 {
		s.hourly(t, "a", 30)
		s.hourly(t, "b", 25)
		s.clock.advance(time.Hour)
		s.maintain(t)
	}
	before, end, _ := s.followAll(t, Cursor{}, 1000)
	var cursors []Cursor
	for cursor := (Cursor{}); ; {
		batch, err := s.Follow(t.Context(), cursor, 7)
		if err != nil || len(batch.Records) == 0 {
			break
		}
		cursors = append(cursors, cursor)
		cursor = batch.Next
	}

	s.hourly(t, "a", 30)
	s.hourly(t, "b", 25)
	s.clock.advance(time.Hour)
	if work := s.maintain(t); work.MergedSegments != 6 {
		t.Fatalf("four hours of two streams merged %d segments", work.MergedSegments)
	}
	added, _, _ := s.followAll(t, end, 1000)
	if len(added) != 55 {
		t.Fatalf("%d records after the merge's places, want 55", len(added))
	}
	for i, cursor := range cursors {
		got, _, expired := s.followAll(t, cursor, 5)
		if expired != 0 {
			t.Fatalf("from %+v: %d expired", cursor, expired)
		}
		sameRecords(t, slices.Concat(before[i*7:], added), got)
	}
}

// a merged segment expires whole, with every place it holds, and a follower
// counts the places it missed
func TestAMergedSegmentExpiresWithItsPlaces(t *testing.T) {
	s := openTestStore(t, t.TempDir(), Options{Retention: 24 * time.Hour}, tinystore.Options{})
	for range 4 {
		s.hourly(t, "quiet", 10)
		s.clock.advance(time.Hour)
		s.maintain(t)
	}
	s.clock.advance(25 * time.Hour)
	if work := s.maintain(t); work.ExpiredSegments != 1 {
		t.Fatalf("expired %+v", work)
	}
	if c := s.counts(t); c.segments != 0 || c.blocks != 0 || c.keys != 0 {
		t.Fatalf("after retention: %+v", c)
	}
	batch, err := s.Follow(t.Context(), Cursor{Segment: 1}, 10)
	if err != nil || batch.Expired != 4 || batch.Next != (Cursor{Segment: 5}) {
		t.Fatalf("following the expired places: %+v, %v", batch, err)
	}
}

// a merged segment that no longer reads is dropped whole, with its places
func TestDropRemovesAMergedSegmentWithItsPlaces(t *testing.T) {
	s := openRecords(t)
	for range 4 {
		s.hourly(t, "quiet", 10)
		s.clock.advance(time.Hour)
		s.maintain(t)
	}
	kept := s.hourly(t, "other", 5)
	s.clock.advance(time.Hour)
	s.maintain(t)
	holder := s.ids(t, `select id from segments where holder is null order by id`)[0]
	s.damage(t, "blocks", s.ids(t, `select id from blocks where segment = ?`, holder)[0])

	_, err := s.Follow(t.Context(), Cursor{Segment: 3}, 100)
	var met *DamageError
	if !errors.As(err, &met) || met.Damage.Segment != holder {
		t.Fatalf("a follow over a place of the damaged segment: %v", err)
	}
	if err = s.Drop(t.Context(), met.Damage); err != nil {
		t.Fatal(err)
	}
	batch, err := s.Follow(t.Context(), Cursor{Segment: 3}, 100)
	if err != nil || batch.Expired != 2 {
		t.Fatalf("following past the dropped places: %+v, %v", batch, err)
	}
	sameRecords(t, kept, batch.Records)
	if c := s.counts(t); c.segments != 1 || c.blocks != 1 {
		t.Fatalf("after the drop: %+v", c)
	}
}

// a merge is one transaction: when it fails the segments are as they were,
// their records read and follow once, and the stream's next seal merges them
func TestAFailedMergeLeavesTheSegmentsAsTheyWere(t *testing.T) {
	s := openRecords(t)
	var all []Record
	for range 3 {
		all = append(all, s.hourly(t, "quiet", 30)...)
		s.clock.advance(time.Hour)
		s.maintain(t)
	}
	s.exec(t, `create trigger refuse before insert on blocks when new.segment = 1
		begin select raise(abort, 'refused'); end`)
	all = append(all, s.hourly(t, "quiet", 30)...)
	s.clock.advance(time.Hour)
	if _, err := s.Maintain(t.Context()); err == nil {
		t.Fatal("a refused merge merged")
	}
	if c := s.counts(t); c.segments != 4 || c.blocks != 4 {
		t.Fatalf("a failed merge changed the file: %+v", c)
	}
	sameRecords(t, sortedByTime(all), s.readAll(t, Query{}))
	followed, _, _ := s.followAll(t, Cursor{}, 1000)
	sameRecords(t, sortedByTime(all), followed)

	s.exec(t, `drop trigger refuse`)
	all = append(all, s.hourly(t, "quiet", 1)...)
	s.clock.advance(time.Hour)
	if work := s.maintain(t); work.MergedSegments != 3 {
		t.Fatalf("the next seal merged %d", work.MergedSegments)
	}
	followed, _, _ = s.followAll(t, Cursor{}, 1000)
	sameRecords(t, sortedByTime(all), followed)
}

func (s *testStore) exec(t *testing.T, statement string) {
	t.Helper()
	err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), statement)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
}

// readers and followers see each record once while a quiet stream is
// appended to, sealed every hour and merged beside them
func TestReadersSeeEveryRecordOnceWhileMerging(t *testing.T) {
	s := openRecords(t)
	const hours = 40
	var appended, reads atomic.Int64
	var work sync.WaitGroup
	work.Go(func() {
		for hour := range hours {
			batch := make([]Record, 20)
			for i := range batch {
				at := s.clock.Now().Add(-time.Minute).Add(time.Duration(i) * time.Millisecond)
				batch[i] = Record{At: at, Stream: "quiet", Name: "tick", Attrs: []Field{Int("i", int64(hour*20+i))}}
			}
			s.append(t, batch...)
			s.clock.advance(time.Hour)
			if _, err := s.Maintain(t.Context()); err != nil {
				t.Error(err)
				return
			}
			appended.Store(int64((hour + 1) * 20))
			for reads.Load() < int64(hour/4) {
				runtime.Gosched()
			}
		}
	})
	for appended.Load() < hours*20 {
		before := int(appended.Load())
		readEveryRecordOnce(t, s, before)
		followEveryRecordOnce(t, s, before)
		reads.Add(1)
	}
	work.Wait()
	if s.Stats().MergedSegments == 0 {
		t.Fatal("nothing merged beside the readers")
	}
}

func followEveryRecordOnce(t *testing.T, s *testStore, before int) {
	t.Helper()
	followed, _, _ := s.followAll(t, Cursor{}, 13)
	seen := map[string]int{}
	for _, record := range followed {
		seen[record.Attrs[0].Value]++
	}
	for i := range before {
		if count := seen[strconv.Itoa(i)]; count != 1 {
			t.Fatalf("record %d followed %d times, with %d sealed before", i, count, before)
		}
	}
}
