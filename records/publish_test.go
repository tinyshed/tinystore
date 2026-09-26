package records

import (
	"slices"
	"testing"
	"time"
)

// a block's id is never given twice, even after a merge deletes the newest
// blocks, so that Follow knows a block's bytes by its id
func TestABlockIDIsNeverGivenTwice(t *testing.T) {
	s := openRecords(t)
	for range 4 {
		s.hourly(t, "quiet", 30)
		s.clock.advance(time.Hour)
		s.maintain(t)
	}
	if ids := s.ids(t, `select id from blocks`); !slices.Equal(ids, []int64{5}) {
		t.Fatalf("the merged block has id %v, want 5, after the four it replaced", ids)
	}
}
