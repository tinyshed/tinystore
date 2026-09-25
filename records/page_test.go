package records

import (
	"testing"
	"time"
)

// the example in the comment on selection.page
func TestAPageEndsBeforeTheFirstTimeItCannotReturnWhole(t *testing.T) {
	q := &checkedQuery{first: 0, last: 10, limit: 3}
	chosen := selection{limit: 3, kept: rankedHeap{}}
	src := &source{block: true, id: 1, segment: 1}
	for row, at := range []int64{1, 2, 2, 2, 3} {
		chosen.offer(&Record{At: time.Unix(0, at), Stream: "web", Name: "tick"}, src, row)
	}
	page, err := chosen.page(q, fetched{})
	if err != nil || len(page.Records) != 1 || !page.More || page.Next.From.UnixNano() != 2 {
		t.Fatalf("records %d, more %v, next from %v, %v", len(page.Records), page.More, page.Next.From, err)
	}
}
