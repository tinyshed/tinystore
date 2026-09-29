package records

import (
	"context"
	"errors"
	"log/slog"
	"slices"
	"sync"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// testClock is the store's clock, moved by the test
type testClock struct {
	mu  sync.Mutex
	now time.Time
}

func (c *testClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *testClock) advance(d time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.now = c.now.Add(d)
}

func (c *testClock) set(now time.Time) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.now = now
}

// testNow is when the fixtures' records happen, so retention keeps them
var testNow = time.Unix(0, fixtureBase).UTC().Add(time.Hour)

type testStore struct {
	*Store
	runtime *tinystore.Store
	clock   *testClock
	dir     string
}

func openTestStore(t *testing.T, dir string, options Options, runtime tinystore.Options) *testStore {
	t.Helper()
	clock := &testClock{now: testNow}
	runtime.Manual, runtime.Clock = true, clock.Now
	store, err := tinystore.Open(t.Context(), dir, runtime)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close(context.Background()) })
	logs, err := Open(t.Context(), store, options)
	if err != nil {
		t.Fatal(err)
	}
	return &testStore{Store: logs, runtime: store, clock: clock, dir: dir}
}

func openRecords(t *testing.T) *testStore {
	t.Helper()
	return openTestStore(t, t.TempDir(), Options{}, tinystore.Options{})
}

// append writes a fixture as few Appends as a segment's input allows
func (s *testStore) append(t testing.TB, records ...Record) {
	t.Helper()
	for _, piece := range appendsOf(records) {
		if err := s.Append(t.Context(), piece...); err != nil {
			t.Fatal(err)
		}
	}
}

func (s *testStore) maintain(t testing.TB) Maintenance {
	t.Helper()
	work, err := s.Maintain(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	return work
}

// readAll pages through a whole query and returns every record, in order
func (s *testStore) readAll(t testing.TB, query Query) []Record {
	t.Helper()
	var all []Record
	for range 10_000 {
		page, err := s.Read(t.Context(), query)
		if err != nil {
			t.Fatal(err)
		}
		all = append(all, page.Records...)
		if !page.More {
			return all
		}
		query = page.Next
	}
	t.Fatal("paging never ended")
	return nil
}

func sortedByTime(records []Record) []Record {
	sorted := slices.Clone(records)
	slices.SortStableFunc(sorted, func(a, b Record) int { return a.At.Compare(b.At) })
	return sorted
}

func TestAppendedRecordsAreReadBeforeTheyAreSealed(t *testing.T) {
	s := openRecords(t)
	records := backendRecords(50)
	s.append(t, records...)

	sameRecords(t, records, s.readAll(t, Query{}))
	if work := s.maintain(t); work.SealedSegments != 0 {
		t.Fatalf("fifty fresh records sealed: %+v", work)
	}
	if stats := s.Stats(); stats.Appended != 50 || stats.Queries == 0 {
		t.Fatalf("stats %+v", stats)
	}
}

func TestRecordsSurviveCloseAndReopen(t *testing.T) {
	dir := t.TempDir()
	s := openTestStore(t, dir, Options{}, tinystore.Options{})
	sealed, waiting := frontendRecords(maxSegmentRecords), backendRecords(100)
	s.append(t, sealed...)
	s.append(t, waiting...)
	if work := s.maintain(t); work.SealedSegments != 1 {
		t.Fatalf("a full head of frontend records did not seal: %+v", work)
	}
	if err := s.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}

	reopened := openTestStore(t, dir, Options{}, tinystore.Options{})
	got := reopened.readAll(t, Query{Streams: []string{"frontend"}})
	sameRecords(t, sortedByTime(sealed), got)
	sameRecords(t, waiting, reopened.readAll(t, Query{Streams: []string{"backend"}}))
}

func TestAppendRefusesARecordAndNamesIt(t *testing.T) {
	s := openRecords(t)
	good := backendRecords(3)
	for name, bad := range map[string]Record{
		"no stream":  {At: testNow, Name: "x"},
		"no name":    {At: testNow, Stream: "x"},
		"not json":   {At: testNow, Stream: "x", Name: "x", Attrs: []Field{{"k", "unquoted"}}},
		"too early":  {At: time.Date(1600, 1, 1, 0, 0, 0, 0, time.UTC), Stream: "x", Name: "x"},
		"too many":   {At: testNow, Stream: "x", Name: "x", Attrs: slices.Repeat([]Field{{"k", "1"}}, maxFields+1)},
		"too large":  {At: testNow, Stream: "x", Name: "x", Body: new(string(make([]byte, maxBlockInput)))},
		"wide level": {At: testNow, Stream: "x", Name: "x", Level: new(slog.Level(maxLevel + 1))},
	} {
		err := s.Append(t.Context(), good[0], good[1], bad, good[2])
		var refused *RecordError
		if !errors.As(err, &refused) || refused.Index != 2 ||
			(!errors.Is(err, tinystore.ErrInvalid) && !errors.Is(err, tinystore.ErrLimit)) {
			t.Errorf("%s: %v", name, err)
		}
	}
	if got := s.readAll(t, Query{}); len(got) != 0 {
		t.Fatalf("a refused batch wrote %d records", len(got))
	}
}

// A record past retention would never be read, and one far ahead of the store's
// clock would hold its segment past retention. Append refuses both and names
// them, writes nothing of their batch, and takes the window's edges.
func TestARecordOutsideItsWindowIsRefused(t *testing.T) {
	s := openTestStore(t, t.TempDir(), Options{Retention: time.Hour, ClockSkew: time.Minute}, tinystore.Options{})
	oldest, newest := testNow.Add(-time.Hour), testNow.Add(time.Minute)
	for name, test := range map[string]struct {
		at   time.Time
		kind error
	}{
		"past retention":     {oldest.Add(-time.Nanosecond), tinystore.ErrTooOld},
		"ahead of the clock": {newest.Add(time.Nanosecond), tinystore.ErrTooNew},
	} {
		err := s.Append(t.Context(), Record{At: testNow, Stream: "x", Name: "x"}, Record{At: test.at, Stream: "x", Name: "x"})
		var refused *RecordError
		if !errors.As(err, &refused) || refused.Index != 1 || !errors.Is(err, test.kind) {
			t.Errorf("%s: %v", name, err)
		}
	}
	if got := s.readAll(t, Query{}); len(got) != 0 {
		t.Fatalf("a refused batch wrote %d records", len(got))
	}
	edges := []Record{{At: oldest, Stream: "x", Name: "oldest"}, {At: newest, Stream: "x", Name: "newest"}}
	s.append(t, edges...)
	sameRecords(t, edges, s.readAll(t, Query{}))
}

// one Append carries at most a segment's input, so that one call's memory is
// bounded whatever the store's; the handler writes a larger flush in pieces
func TestAnAppendOfMoreThanASegmentIsRefused(t *testing.T) {
	s := openLogging(t, t.TempDir(), Options{Buffer: 32})
	body := string(make([]byte, 200<<10))
	lines := make([]Record, 21)
	for i := range lines {
		lines[i] = Record{At: s.clock.Now(), Stream: "x", Name: "x", Body: &body}
	}
	if err := s.Append(t.Context(), lines...); !errors.Is(err, tinystore.ErrLimit) {
		t.Fatalf("an Append of 4.2 MB: %v", err)
	}
	if got := s.readAll(t, Query{}); len(got) != 0 {
		t.Fatalf("a refused Append wrote %d records", len(got))
	}
	logger := slog.New(s.Handler("app"))
	for range lines {
		logger.Info(body)
	}
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if stats := s.Stats(); stats.Appended != uint64(len(lines)) || stats.Dropped != 0 {
		t.Fatalf("a flush of 4.2 MB: %+v", stats)
	}
}

func TestAClosedStoreRefusesWork(t *testing.T) {
	s := openRecords(t)
	if err := s.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := s.Append(t.Context(), backendRecords(1)...); !errors.Is(err, tinystore.ErrClosed) {
		t.Errorf("append after close: %v", err)
	}
	if _, err := s.Read(t.Context(), Query{}); !errors.Is(err, tinystore.ErrClosed) {
		t.Errorf("read after close: %v", err)
	}
}
