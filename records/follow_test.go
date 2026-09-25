package records

import (
	"errors"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// followAll reads every sealed record from a cursor on, a batch at a time
func (s *testStore) followAll(t *testing.T, from Cursor, limit int) ([]Record, Cursor, int) {
	t.Helper()
	var all []Record
	expired := 0
	for {
		batch, err := s.Follow(t.Context(), from, limit)
		if err != nil {
			t.Fatal(err)
		}
		all, expired = append(all, batch.Records...), expired+batch.Expired
		if len(batch.Records) == 0 {
			return all, batch.Next, expired
		}
		from = batch.Next
	}
}

// a consumer reads sealed segments in the order they were sealed, each in
// event-time order, whatever its batch size, and never sees the head
func TestFollowReadsSealedSegmentsInOrder(t *testing.T) {
	s := openRecords(t)
	first, second := frontendRecords(maxSegmentRecords), backendRecords(maxSegmentRecords)
	s.append(t, first...)
	s.maintain(t)
	s.append(t, second...)
	s.maintain(t)
	s.append(t, backendRecords(5)...)

	want := append(sortedByTime(first), sortedByTime(second)...)
	for _, limit := range []int{1000, 777, 10_000} {
		got, next, expired := s.followAll(t, Cursor{}, limit)
		sameRecords(t, want, got)
		if next != (Cursor{Segment: 3}) || expired != 0 {
			t.Fatalf("limit %d ended at %+v with %d expired", limit, next, expired)
		}
	}
	got, _, _ := s.followAll(t, Cursor{Segment: 2, Row: maxSegmentRecords - 3}, 2)
	sameRecords(t, want[2*maxSegmentRecords-3:], got)
}

// a cursor retention has passed is told how many segments it missed
func TestFollowCountsWhatRetentionRemovedFirst(t *testing.T) {
	s := openTestStore(t, t.TempDir(), Options{Retention: time.Hour}, tinystore.Options{})
	for range 3 {
		s.append(t, backendRecords(maxSegmentRecords)...)
		s.maintain(t)
	}
	batch, err := s.Follow(t.Context(), Cursor{Segment: 1, Row: 5}, 10)
	if err != nil || batch.Expired != 0 || len(batch.Records) != 10 {
		t.Fatalf("before retention: %d records, %d expired, %v", len(batch.Records), batch.Expired, err)
	}
	s.clock.advance(2 * time.Hour)
	s.maintain(t)
	batch, err = s.Follow(t.Context(), batch.Next, 10)
	if err != nil || batch.Expired != 3 || len(batch.Records) != 0 || batch.Next != (Cursor{Segment: 4}) {
		t.Fatalf("after retention: %+v, %v", batch, err)
	}
}

func TestFollowRefusesABadCursor(t *testing.T) {
	s := openRecords(t)
	if _, err := s.Follow(t.Context(), Cursor{Segment: -1}, 10); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a negative cursor: %v", err)
	}
	if _, err := s.Follow(t.Context(), Cursor{}, maxLimit+1); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("an unbounded batch: %v", err)
	}
}
