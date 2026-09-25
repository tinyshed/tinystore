package records

import (
	"testing"
	"time"
)

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
