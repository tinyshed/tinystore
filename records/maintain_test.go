package records

import (
	"database/sql"
	"errors"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// counts is what the tables hold, read in one snapshot
type counts struct {
	heads, states, segments, blocks, keys int
}

func (s *testStore) counts(t *testing.T) counts {
	t.Helper()
	var c counts
	err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select
			(select count(*) from heads), (select count(*) from head_state), (select count(*) from segments),
			(select count(*) from blocks), (select count(*) from segment_keys)`).
			Scan(&c.heads, &c.states, &c.segments, &c.blocks, &c.keys)
	})
	if err != nil {
		t.Fatal(err)
	}
	return c
}

// a full segment seals at once; what is left waits for SealAge, counted from
// when its oldest row was written, not from its records' times
func TestAHeadSealsWhenFullOrOld(t *testing.T) {
	s := openRecords(t)
	records := frontendRecords(maxSegmentRecords + 100)
	s.append(t, records...)

	if work := s.maintain(t); work.SealedSegments != 1 || work.SealedRecords > maxSegmentRecords {
		t.Fatalf("a segment's worth and a hundred more: %+v", work)
	}
	s.clock.advance(59 * time.Minute)
	if work := s.maintain(t); work.SealedSegments != 0 {
		t.Fatalf("the rest sealed before an hour: %+v", work)
	}
	s.clock.advance(time.Minute)
	if work := s.maintain(t); work.SealedSegments != 1 {
		t.Fatalf("the rest did not seal after an hour: %+v", work)
	}
	if c := s.counts(t); c.heads != 0 || c.states != 0 || c.segments != 2 {
		t.Fatalf("after sealing everything: %+v", c)
	}
	sameRecords(t, sortedByTime(records), s.readAll(t, Query{}))
}

// a record more than ten seconds behind its stream goes to the stream's late
// head and seals into a segment of its own
func TestLateRecordsSealFromTheirOwnHead(t *testing.T) {
	s := openRecords(t)
	records := frontendRecords(1000)
	records[500].At = records[500].At.Add(-10 * time.Minute)
	s.append(t, records...)

	var late int
	err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select sum(count) from head_state where late = 1`).Scan(&late)
	})
	if err != nil || late != 1 {
		t.Fatalf("%d records in the late head, want 1: %v", late, err)
	}
	s.clock.advance(time.Hour)
	if work := s.maintain(t); work.SealedSegments != 2 {
		t.Fatalf("on-time and late heads: %+v", work)
	}
	var lateSpan int64
	err = s.file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select max(last_at - first_at) from blocks`).Scan(&lateSpan)
	})
	if err != nil || time.Duration(lateSpan) > 3*time.Second {
		t.Fatalf("the widest block spans %s; the late record stretched one", time.Duration(lateSpan))
	}
	sameRecords(t, sortedByTime(records), s.readAll(t, Query{}))
}

// sealing is one transaction: when it fails the head is as it was, its
// records still read once, and the next pass seals them
func TestAFailedSealLeavesTheHeadAsItWas(t *testing.T) {
	s := openRecords(t)
	records := frontendRecords(maxSegmentRecords)
	s.append(t, records...)
	before := s.counts(t)
	refuse := `create trigger refuse before insert on blocks when new.id > 1 begin select raise(abort, 'refused'); end`
	if err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), refuse)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := s.Maintain(t.Context()); err == nil {
		t.Fatal("a refused publication sealed")
	}
	if after := s.counts(t); after != before {
		t.Fatalf("a failed seal changed the file: %+v, was %+v", after, before)
	}
	sameRecords(t, sortedByTime(records), s.readAll(t, Query{}))

	if err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), `drop trigger refuse`)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if work := s.maintain(t); work.SealedSegments != 1 {
		t.Fatalf("the seal after the failure: %+v", work)
	}
	sameRecords(t, sortedByTime(records), s.readAll(t, Query{}))
}

// a head row that no longer reads is counted and left, and the others seal
func TestADamagedHeadDoesNotStopTheOthers(t *testing.T) {
	s := openRecords(t)
	s.append(t, frontendRecords(100)...)
	s.append(t, backendRecords(100)...)
	damage := `update heads set body = cast(substr(body, 1, 20) || x'ff' || substr(body, 22) as blob)
		where stream = (select id from streams where name = 'frontend')`
	if err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), damage)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	s.clock.advance(time.Hour)
	work, err := s.Maintain(t.Context())
	if err != nil || work.SealedSegments != 1 || work.Damaged != 1 {
		t.Fatalf("one damaged head: %+v, %v", work, err)
	}
	sameRecords(t, backendRecords(100), s.readAll(t, Query{Streams: []string{"backend"}}))
}

// Retention removes whole segments, their blocks, filters and keys, and head
// rows. A read never returns a record past the cutoff, even from a segment
// retention has only partly passed.
func TestRetentionRemovesWholeSegmentsAndClipsReads(t *testing.T) {
	s := openTestStore(t, t.TempDir(), Options{Retention: time.Hour}, tinystore.Options{})
	records := backendRecords(2 * maxSegmentRecords)
	s.append(t, records...)
	s.maintain(t)
	waiting := backendRecords(10)
	for i := range waiting {
		waiting[i].At = records[len(records)-1].At.Add(time.Duration(i) * time.Millisecond)
	}
	s.append(t, waiting...)
	records = append(records, waiting...)
	span := records[len(records)-1].At.Sub(records[0].At)

	s.clock.advance(span/4 + time.Hour - testNow.Sub(records[0].At))
	cutoff := s.clock.Now().Add(-time.Hour)
	var kept []Record
	for _, record := range records {
		if !record.At.Before(cutoff) {
			kept = append(kept, record)
		}
	}
	sameRecords(t, kept, s.readAll(t, Query{}))
	if work := s.maintain(t); work.ExpiredSegments != 0 {
		t.Fatalf("a segment still holding kept records expired: %+v", work)
	}

	s.clock.advance(span)
	work := s.maintain(t)
	if c := s.counts(t); work.ExpiredSegments != 2 || work.ExpiredHeads != 1 || c != (counts{}) {
		t.Fatalf("after everything expired: %+v, tables %+v", work, c)
	}
	if stats := s.Stats(); stats.ExpiredSegments != 2 {
		t.Fatalf("stats %+v", stats)
	}
}

// A stream RetentionOf names keeps its records for its own duration: Append,
// reads and expiry each take its cutoff, while every other stream keeps
// Retention's. audit keeps three hours, api one.
func TestAStreamKeepsItsOwnRetention(t *testing.T) {
	s := openTestStore(t, t.TempDir(), Options{
		Retention: time.Hour, RetentionOf: map[string]time.Duration{"audit": 3 * time.Hour},
	}, tinystore.Options{})
	now := s.clock.Now()
	at := func(ago time.Duration) time.Time { return now.Add(-ago) }
	s.append(t,
		Record{Stream: "audit", Name: "login", At: at(150 * time.Minute)},
		Record{Stream: "audit", Name: "login", At: at(10 * time.Minute)},
		Record{Stream: "api", Name: "request", At: at(50 * time.Minute)},
	)
	err := s.Append(t.Context(), Record{Stream: "api", Name: "request", At: at(90 * time.Minute)})
	if !errors.Is(err, tinystore.ErrTooOld) {
		t.Fatalf("a record past api's hour: %v", err)
	}
	if got := s.readAll(t, Query{}); len(got) != 3 {
		t.Fatalf("every stream inside its own retention: %d records", len(got))
	}

	s.clock.advance(time.Hour) // the first audit record is 3.5h old, the api one 1h50m
	got := s.readAll(t, Query{})
	if len(got) != 1 || got[0].Stream != "audit" || !got[0].At.Equal(at(10*time.Minute)) {
		t.Fatalf("each stream past its own retention: %+v", got)
	}
	if work := s.maintain(t); work.ExpiredHeads == 0 {
		t.Fatalf("api's records past its hour did not expire: %+v", work)
	}
	if got = s.readAll(t, Query{Streams: []string{"audit"}}); len(got) != 1 {
		t.Fatalf("audit after expiry: %+v", got)
	}
}
