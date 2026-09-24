package metrics

import (
	"context"
	"database/sql"
	"testing"
	"time"
)

func TestExpiredSeriesReclaimsCardinalityAndAllowsNewLifecycle(t *testing.T) {
	s, path := openTestStore(t, Options{MaxSeries: 1, Retention: time.Second})
	old := Series{Kind: Counter, Labels: []Label{{Name: "__name__", Value: "cpu"}, {Name: "host", Value: "old"}}}
	if err := s.Ingest(t.Context(), []Batch{{Series: old, Samples: testSamples(241)}}); err != nil {
		t.Fatal(err)
	}
	if result, err := s.Maintain(t.Context()); err != nil || result.SealedBlocks != 1 {
		t.Fatalf("seal old lifecycle: %+v, %v", result, err)
	}
	s.now = func() time.Time { return time.UnixMilli(testEpoch + 3000) }
	result, err := s.Maintain(t.Context())
	if err != nil || result.ExpiredSamples != 241 || result.ReclaimedSeries != 1 || s.Stats().ReclaimedSeries != 1 {
		t.Fatalf("reclaim old lifecycle: %+v, %v, %+v", result, err, s.Stats())
	}
	if err = s.file.View(t.Context(), func(tx *sql.Tx) error {
		var series, postings, labels, groups, count int
		if readErr := tx.QueryRowContext(t.Context(), `select (select count(*) from series),(select count(*) from postings),(select count(*) from label_values),(select count(*) from groups),(select series_count from store_state where id=1)`).Scan(&series, &postings, &labels, &groups, &count); readErr != nil {
			return readErr
		}
		if series != 0 || postings != 0 || labels != 0 || groups != 0 || count != 0 {
			t.Fatalf("expired registration remains: series=%d postings=%d labels=%d groups=%d count=%d", series, postings, labels, groups, count)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	newSeries := Series{Labels: []Label{{Name: "__name__", Value: "new"}}}
	if err = s.Ingest(t.Context(), []Batch{{Series: newSeries, Samples: []Sample{{At: testEpoch + 3000, Value: 1}}}}); err != nil {
		t.Fatalf("new series after reclaim: %v", err)
	}
	s.now = func() time.Time { return time.UnixMilli(testEpoch + 5000) }
	if result, maintainErr := s.Maintain(t.Context()); maintainErr != nil || result.ReclaimedSeries != 1 {
		t.Fatalf("reclaim replacement: %+v, %v", result, maintainErr)
	}
	old.Kind = Gauge
	if err = s.Ingest(t.Context(), []Batch{{Series: old, Samples: []Sample{{At: testEpoch + 5000, Value: 42}}}}); err != nil {
		t.Fatalf("new kind after complete expiry: %v", err)
	}
	if err = s.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	reopened, err := Open(t.Context(), path, Options{MaxSeries: 1, Retention: time.Second})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = reopened.Close(context.Background()) })
	reopened.now = func() time.Time { return time.UnixMilli(testEpoch + 5000) }
	read, err := reopened.Read(t.Context(), Range{Matchers: old.Labels, From: testEpoch + 5000, To: testEpoch + 5001})
	if err != nil || len(read) != 1 || read[0].Series.Kind != Gauge || len(read[0].Samples) != 1 || read[0].Samples[0].Value != 42 {
		t.Fatalf("reopened lifecycle: %+v, %v", read, err)
	}
}

func TestReclaimKeepsLabelsUsedByAnotherSeries(t *testing.T) {
	s, _ := openTestStore(t, Options{MaxSeries: 2, Retention: time.Second})
	a := Series{Labels: []Label{{Name: "__name__", Value: "cpu"}, {Name: "host", Value: "old"}}}
	b := Series{Labels: []Label{{Name: "__name__", Value: "cpu"}, {Name: "host", Value: "live"}}}
	if err := s.Ingest(t.Context(), []Batch{{Series: a, Samples: []Sample{{At: testEpoch, Value: 1}}}, {Series: b, Samples: []Sample{{At: testEpoch + 1000, Value: 2}}}}); err != nil {
		t.Fatal(err)
	}
	s.now = func() time.Time { return time.UnixMilli(testEpoch + 1501) }
	result, err := s.Maintain(t.Context())
	if err != nil || result.ReclaimedSeries != 1 {
		t.Fatalf("reclaim shared label: %+v, %v", result, err)
	}
	if err = s.file.View(t.Context(), func(tx *sql.Tx) error {
		var count, oldHost int
		if readErr := tx.QueryRowContext(t.Context(), `select posting_count from label_values where name='__name__' and value='cpu'`).Scan(&count); readErr != nil {
			return readErr
		}
		if readErr := tx.QueryRowContext(t.Context(), `select count(*) from label_values where name='host' and value='old'`).Scan(&oldHost); readErr != nil {
			return readErr
		}
		if count != 1 || oldHost != 0 {
			t.Fatalf("shared label count=%d, removed host entries=%d", count, oldHost)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	read, err := s.Read(t.Context(), Range{Matchers: b.Labels, From: testEpoch + 1000, To: testEpoch + 1001})
	if err != nil || len(read) != 1 || len(read[0].Samples) != 1 {
		t.Fatalf("live series after sibling reclaim: %+v, %v", read, err)
	}
}
