package records

import (
	"database/sql"
	"errors"
	"log/slog"
	"runtime"
	"slices"
	"strconv"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// segments that overlap in time, and records still in the head, come back as
// one sequence in event-time order, equal times in the order they arrived
func TestReadMergesSegmentsAndTheHeadInEventTimeOrder(t *testing.T) {
	s := openRecords(t)
	records := frontendRecords(3 * maxSegmentRecords)
	var appended []Record
	for i, record := range records {
		if i%7 == 0 {
			record.Stream = "mobile"
		}
		appended = append(appended, record)
	}
	s.append(t, appended...)
	if work := s.maintain(t); work.SealedSegments < 2 {
		t.Fatalf("nothing overlapping was sealed: %+v", work)
	}
	late := slices.Clone(appended[:10])
	for i := range late {
		late[i].Name = "repeat"
	}
	s.append(t, late...)

	want := sortedByTime(append(slices.Clone(appended), late...))
	got := s.readAll(t, Query{Limit: 777})
	sameTimes(t, want, got)
	for i := 1; i < len(got); i++ {
		if got[i].At.Equal(got[i-1].At) && got[i-1].Name == "repeat" && got[i].Name != "repeat" &&
			got[i].Stream == got[i-1].Stream {
			t.Fatalf("record %d arrived before the one it follows at the same time", i)
		}
	}
}

// sameTimes compares two sequences by time and content, whatever order equal
// times from different streams take
func sameTimes(t *testing.T, want, got []Record) {
	t.Helper()
	if len(want) != len(got) {
		t.Fatalf("%d records, want %d", len(got), len(want))
	}
	for i := range want {
		if !want[i].At.Equal(got[i].At) {
			t.Fatalf("record %d at %s, want %s", i, got[i].At, want[i].At)
		}
	}
}

// paging through a range oldest first and newest first returns every record
// once, whatever the limit, and never splits a timestamp
func TestPagesContinueWithoutLosingOrRepeating(t *testing.T) {
	s := openRecords(t)
	records := frontendRecords(2000)
	for i := range records {
		if i%3 == 0 {
			records[i].At = records[i-i%9].At
		}
	}
	s.append(t, records[:1600]...)
	s.clock.advance(2 * time.Hour)
	s.maintain(t)
	s.append(t, records[1600:]...)
	want := sortedByTime(records)

	for _, limit := range []int{3, 7, 1000} {
		sameTimes(t, want, s.readAll(t, Query{Limit: limit}))
		newest := s.readAll(t, Query{Limit: limit, Newest: true})
		slices.Reverse(newest)
		sameTimes(t, want, newest)
	}
}

func TestAPageNeverSplitsATimestamp(t *testing.T) {
	s := openRecords(t)
	at := testNow.Add(-time.Minute)
	var records []Record
	for i := range 10 {
		records = append(records, Record{At: at.Add(time.Duration(i/4) * time.Second), Stream: "web", Name: "tick"})
	}
	s.append(t, records...)

	page, err := s.Read(t.Context(), Query{Limit: 6})
	if err != nil || len(page.Records) != 4 || !page.More {
		t.Fatalf("a limit of 6 over times of 4, 4 and 2 records: %d records, more %v, %v",
			len(page.Records), page.More, err)
	}
	if _, err = s.Read(t.Context(), Query{Limit: 3}); !errors.Is(err, tinystore.ErrLimit) {
		t.Fatalf("four records at one time under a limit of 3: %v", err)
	}
}

// a budget that cannot hold the range ends the page where the blocks it took
// stop covering every record, and the next page goes on from there
func TestABudgetEndsAPageEarly(t *testing.T) {
	s := openRecords(t)
	records := frontendRecords(2 * maxSegmentRecords)
	s.append(t, records...)
	s.maintain(t)

	query := Query{Budget: Budget{Blocks: 3}}
	page, err := s.Read(t.Context(), query)
	if err != nil || !page.More || len(page.Records) == 0 || len(page.Records) > 3*maxBlockRecords {
		t.Fatalf("three blocks' budget: %d records, more %v, %v", len(page.Records), page.More, err)
	}
	sameTimes(t, sortedByTime(records), s.readAll(t, query))
	if _, err = s.Read(t.Context(), Query{Budget: Budget{Bytes: 100}}); !errors.Is(err, tinystore.ErrLimit) {
		t.Fatalf("a budget smaller than one block: %v", err)
	}
	if _, err = s.Read(t.Context(), Query{Budget: Budget{Blocks: 1 << 30}}); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a budget wider than the store's: %v", err)
	}
}

func TestReadFiltersByEveryCondition(t *testing.T) {
	s := openRecords(t)
	backend, frontend := backendRecords(4000), frontendRecords(4000)
	s.append(t, backend...)
	s.append(t, frontend...)
	s.clock.advance(2 * time.Hour)
	s.maintain(t)
	// the ends of time are outside any store's window; the segment tests keep them
	edge := slices.DeleteFunc(edgeRecords(), func(r Record) bool { return r.Name == "ends of time" })
	s.append(t, edge...)

	errorLevel := slog.LevelError
	session := frontend[123].Context[4]
	cases := map[string]struct {
		query Query
		keep  func(*Record) bool
	}{
		"stream":   {Query{Streams: []string{"edge"}}, func(r *Record) bool { return r.Stream == "edge" }},
		"no such":  {Query{Streams: []string{"nothing"}}, func(*Record) bool { return false }},
		"name":     {Query{Names: []string{"values", "unicode"}}, func(r *Record) bool { return r.Name == "values" || r.Name == "unicode" }},
		"level":    {Query{MinLevel: &errorLevel}, func(r *Record) bool { return r.Level != nil && *r.Level >= errorLevel }},
		"trace":    {Query{TraceID: backend[777].TraceID}, func(r *Record) bool { return r.TraceID == backend[777].TraceID }},
		"session":  {Query{Context: []Field{session}}, func(r *Record) bool { return slices.Contains(r.Context, session) }},
		"attrs":    {Query{Attrs: []Field{{"element", `"buy"`}, {"url", `"/catalog"`}}}, hasAttrs(Field{"element", `"buy"`}, Field{"url", `"/catalog"`})},
		"repeated": {Query{Attrs: []Field{{"tag", `"y"`}}}, hasAttrs(Field{"tag", `"y"`})},
		"range": {Query{From: frontend[1000].At, To: frontend[1100].At}, func(r *Record) bool {
			return !r.At.Before(frontend[1000].At) && r.At.Before(frontend[1100].At)
		}},
	}
	cutoff := s.clock.Now().Add(-14 * 24 * time.Hour)
	all := slices.DeleteFunc(sortedByTime(slices.Concat(backend, frontend, edge)), func(r Record) bool {
		return r.At.Before(cutoff)
	})
	for name, test := range cases {
		t.Run(name, func(t *testing.T) {
			var want []Record
			for i := range all {
				if test.keep(&all[i]) {
					want = append(want, all[i])
				}
			}
			sameTimes(t, want, s.readAll(t, test.query))
		})
	}
}

func hasAttrs(fields ...Field) func(*Record) bool {
	return func(r *Record) bool {
		for _, field := range fields {
			if !slices.Contains(r.Attrs, field) {
				return false
			}
		}
		return true
	}
}

// a lookup by an id-like value, a trace or a level fetches only the blocks
// whose bloom or level mask may hold it
func TestBloomsAndLevelMasksSkipBlocks(t *testing.T) {
	s := openRecords(t)
	backend := backendRecords(20 * maxBlockRecords)
	for i := range backend {
		backend[i].Attrs = append(backend[i].Attrs, String("request_id", string(backend[i].TraceID[:8])+"x"))
		if i >= 3*maxBlockRecords {
			backend[i].Level = new(slog.LevelInfo)
		}
	}
	s.append(t, backend...)
	s.clock.advance(2 * time.Hour)
	s.maintain(t)

	target := backend[5000]
	errorLevel := slog.LevelError
	var all, withErrors int
	err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select count(*), sum(levels & ? != 0) from blocks`,
			levelsFrom(int64(errorLevel))).Scan(&all, &withErrors)
	})
	if err != nil || withErrors == 0 || withErrors >= all/2 {
		t.Fatalf("%d of %d blocks hold errors, %v", withErrors, all, err)
	}
	for name, test := range map[string]struct {
		query Query
		most  int
	}{
		"request id": {Query{Attrs: []Field{target.Attrs[len(target.Attrs)-1]}}, 2},
		"trace":      {Query{TraceID: target.TraceID}, 2},
		"errors":     {Query{MinLevel: &errorLevel}, withErrors},
	} {
		checked, err := s.checkQuery(test.query)
		if err != nil {
			t.Fatal(err)
		}
		fetched, err := s.fetchSnapshot(t.Context(), &checked)
		if err != nil {
			t.Fatal(err)
		}
		if blocks := len(fetched.sources); blocks == 0 || blocks > test.most {
			t.Errorf("%s: %d of %d blocks fetched, want 1 to %d", name, blocks, all, test.most)
		}
	}
}

func TestAQueryValueMustBeJSON(t *testing.T) {
	s := openRecords(t)
	if _, err := s.Read(t.Context(), Query{Attrs: []Field{{"route", "/notes"}}}); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("an unquoted string: %v", err)
	}
	if _, err := s.Read(t.Context(), Query{From: testNow, To: testNow.Add(-time.Second)}); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("an inverted range: %v", err)
	}
}

// a reader sees each record appended before it began exactly once while a
// writer appends and maintenance seals: a record is in the head or in its
// segment, never in both and never in neither
func TestReadersSeeEveryRecordOnceWhileSealing(t *testing.T) {
	s := openRecords(t)
	const batches = 3 * maxSegmentRecords / maxBlockRecords
	var appended, reads atomic.Int64
	var work sync.WaitGroup
	work.Go(func() {
		for batch := range batches {
			s.append(t, numberedRecords(batch*maxBlockRecords, maxBlockRecords)...)
			appended.Store(int64((batch + 1) * maxBlockRecords))
			for reads.Load() < int64(batch/2) {
				runtime.Gosched()
			}
		}
	})
	work.Go(func() {
		for appended.Load() < batches*maxBlockRecords {
			if _, err := s.Maintain(t.Context()); err != nil {
				t.Error(err)
				return
			}
		}
	})
	for appended.Load() < batches*maxBlockRecords {
		readEveryRecordOnce(t, s, int(appended.Load()))
		reads.Add(1)
	}
	work.Wait()
	if c := s.counts(t); c.segments < 2 || reads.Load() < batches/2 {
		t.Fatalf("%d reads beside %d sealed segments", reads.Load(), c.segments)
	}
}

func numberedRecords(from, count int) []Record {
	batch := make([]Record, count)
	for i := range batch {
		at := testNow.Add(-time.Hour).Add(time.Duration(from+i) * time.Millisecond)
		batch[i] = Record{At: at, Stream: "web", Name: "tick", Attrs: []Field{Int("i", int64(from+i))}}
	}
	return batch
}

func readEveryRecordOnce(t *testing.T, s *testStore, before int) {
	t.Helper()
	seen := map[string]int{}
	for _, record := range s.readAll(t, Query{Limit: maxLimit}) {
		seen[record.Attrs[0].Value]++
	}
	for i := range before {
		if count := seen[strconv.Itoa(i)]; count != 1 {
			t.Fatalf("record %d read %d times, with %d appended before the read began", i, count, before)
		}
	}
}
