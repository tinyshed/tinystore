package records

import (
	"context"
	"database/sql"
	"fmt"
	"os"
	"runtime"
	"slices"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// the page the previous Read built from every candidate before its budget
func TestReadCandidatesMeasured(t *testing.T) {
	measureReadCandidates(t, 5000, false)
}

func TestReadSealedCandidatesMeasured(t *testing.T) {
	measureReadCandidates(t, 1000, true)
}

func measureReadCandidates(t *testing.T, rows int, sealed bool) {
	t.Helper()
	if os.Getenv("TINYSTORE_SPIKE") != "1" {
		t.Skip("a measurement: set TINYSTORE_SPIKE=1")
	}
	root := os.Getenv("TINYSTORE_RECORDS_DIR")
	if root == "" {
		root = t.TempDir()
	}
	dir, err := os.MkdirTemp(root, "read-round-")
	if err != nil {
		t.Fatal(err)
	}
	s := openTestStore(t, dir, Options{}, tinystore.Options{})
	defer func() {
		_ = s.runtime.Close(context.Background())
		_ = os.RemoveAll(dir)
	}()
	for start := 0; start < rows; start += 200 {
		batch := make([]Record, 0, 200)
		for i := start; i < start+200; i++ {
			body := "a short line"
			batch = append(batch, Record{
				At:     testNow.Add(time.Duration(i) * time.Millisecond),
				Stream: fmt.Sprintf("stream-%05d", i), Name: "line", Body: &body,
			})
		}
		if appendErr := s.Append(t.Context(), batch...); appendErr != nil {
			t.Fatal(appendErr)
		}
	}
	where := "head"
	if sealed {
		where = "sealed"
		s.clock.advance(2 * time.Hour)
		if _, maintainErr := s.Maintain(t.Context()); maintainErr != nil {
			t.Fatal(maintainErr)
		}
	}
	for _, scenario := range []struct {
		name  string
		query Query
	}{
		{"broad oldest", Query{
			From: testNow, To: testNow.Add(time.Duration(rows) * time.Millisecond),
			Limit: 100, Budget: Budget{Blocks: 4},
		}},
		{"broad newest", Query{
			From: testNow, To: testNow.Add(time.Duration(rows) * time.Millisecond),
			Newest: true, Limit: 100, Budget: Budget{Blocks: 4},
		}},
		{"narrow oldest", Query{
			From: testNow.Add(500 * time.Millisecond),
			To:   testNow.Add(510 * time.Millisecond), Limit: 100, Budget: Budget{Blocks: 4},
		}},
	} {
		checked, checkErr := s.checkQuery(scenario.query)
		if checkErr != nil {
			t.Fatal(checkErr)
		}
		snapshot := func() error {
			_, fetchErr := s.fetchSnapshot(context.Background(), &checked)
			return fetchErr
		}
		read := func() error {
			page, readErr := s.Read(context.Background(), scenario.query)
			if readErr == nil && (len(page.Records) != 4 || !page.More) {
				return fmt.Errorf("page has %d records, more=%t", len(page.Records), page.More)
			}
			return readErr
		}
		for _, measure := range []struct {
			name string
			call func() error
		}{{"snapshot", snapshot}, {"read", read}} {
			if callErr := measure.call(); callErr != nil {
				t.Fatal(callErr)
			}
			median, p95, allocated := measureRecordsRead(t, 20, measure.call)
			t.Logf("%s %s %d %s rows, four-block budget: p50 %v p95 %v allocated %d bytes/call",
				scenario.name, measure.name, rows, where, median, p95, allocated)
		}
	}
	err = s.file.View(t.Context(), func(tx *sql.Tx) error {
		var pages, free, pageSize int64
		if scanErr := tx.QueryRowContext(t.Context(), `pragma page_count`).Scan(&pages); scanErr != nil {
			return scanErr
		}
		if scanErr := tx.QueryRowContext(t.Context(), `pragma freelist_count`).Scan(&free); scanErr != nil {
			return scanErr
		}
		if scanErr := tx.QueryRowContext(t.Context(), `pragma page_size`).Scan(&pageSize); scanErr != nil {
			return scanErr
		}
		t.Logf("logical file: %d bytes (%d pages of %d, %d free)", pages*pageSize, pages, pageSize, free)
		rows, queryErr := tx.QueryContext(t.Context(), `select name, sum(pgsize) from dbstat group by name order by name`)
		if queryErr != nil {
			return queryErr
		}
		defer rows.Close()
		for rows.Next() {
			var name string
			var size int64
			if scanErr := rows.Scan(&name, &size); scanErr != nil {
				return scanErr
			}
			t.Logf("dbstat %s: %d bytes", name, size)
		}
		return rows.Err()
	})
	if err != nil {
		t.Fatal(err)
	}
}

func measureRecordsRead(t *testing.T, calls int, call func() error) (time.Duration, time.Duration, uint64) {
	t.Helper()
	runtime.GC()
	var before, after runtime.MemStats
	runtime.ReadMemStats(&before)
	times := make([]time.Duration, calls)
	for i := range times {
		started := time.Now()
		if err := call(); err != nil {
			t.Fatal(err)
		}
		times[i] = time.Since(started)
	}
	runtime.ReadMemStats(&after)
	slices.Sort(times)
	return times[calls/2], times[calls*95/100], (after.TotalAlloc - before.TotalAlloc) / uint64(calls)
}
