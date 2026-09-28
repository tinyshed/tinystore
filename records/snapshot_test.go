package records

import (
	"fmt"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

func TestBroadCandidateWalkStopsAtTheBlockBudget(t *testing.T) {
	s := openRecords(t)
	const count = 300
	for start := 0; start < count; start += 100 {
		var batch []Record
		for i := start; i < start+100; i++ {
			batch = append(batch, Record{
				At:     testNow.Add(time.Duration(i) * time.Millisecond),
				Stream: fmt.Sprintf("stream-%03d", i), Name: "event",
			})
		}
		s.append(t, batch...)
	}
	for _, sealed := range []bool{false, true} {
		if sealed {
			s.clock.advance(2 * time.Hour)
			s.maintain(t)
		}
		for _, newest := range []bool{false, true} {
			query, err := s.checkQuery(Query{
				From: testNow, To: testNow.Add(time.Second),
				Newest: newest, Budget: Budget{Blocks: 2},
			})
			if err != nil {
				t.Fatal(err)
			}
			var found []source
			err = s.file.ViewPrepared(t.Context(), func(tx sqlite.Reader) (readErr error) {
				read := snapshotRead{tx: tx, query: &query, names: &s.streams, spans: &s.spans}
				found, readErr = read.candidates(t.Context())
				return readErr
			})
			if err != nil || len(found) != 3 {
				t.Fatalf("sealed=%t newest=%t: %d candidates: %v", sealed, newest, len(found), err)
			}
		}
	}
}

// the example in the comment on enough
func TestAPlainPageStopsFetchingAtAboutAPage(t *testing.T) {
	second := int64(time.Second)
	read := snapshotRead{query: &checkedQuery{first: 0, last: 100 * second, limit: 100}}
	taken := []source{{first: 0, last: 10*second - 1, count: 1000}}
	if !read.enough(taken, source{first: 2 * second}) {
		t.Error("200 records expected before the next block, and the page stopped not")
	}
	if read.enough(taken, source{first: second / 20}) {
		t.Error("5 records expected before the next block, and the page stopped")
	}
	read.query.asked.Names = []string{"click"}
	if read.enough(taken, source{first: 2 * second}) {
		t.Error("a query that filters rows cannot tell how many match, and stopped")
	}
}
