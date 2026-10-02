package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"path/filepath"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

type countingReader struct {
	sqlite.Reader
	calls int
}

func (r *countingReader) QueryContext(ctx context.Context, query string, arguments ...any) (*sql.Rows, error) {
	r.calls++
	return r.Reader.QueryContext(ctx, query, arguments...)
}

// the driving matcher decides how many posting rows the match scans, and a
// matcher ordered by label name drove from `__name__` at any cardinality
func TestTheShortestPostingListDrivesTheMatch(t *testing.T) {
	store, _ := openTestStore(t, Options{Retention: time.Hour})
	const crowd = 200
	for i := range crowd {
		series := Series{Name: "cpu", Labels: Labels{"host": fmt.Sprintf("host_%d", i)}, Kind: Gauge}
		if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: []Sample{{At: testEpoch, Value: 1}}}}); err != nil {
			t.Fatal(err)
		}
	}
	matchers, _, err := canonicalLabels([]label{{Name: "__name__", Value: "cpu"}, {Name: "host", Value: "host_7"}}, false)
	if err != nil {
		t.Fatal(err)
	}
	if matchers[0].Name != "__name__" {
		t.Fatalf("canonical order no longer starts at __name__, so this test guards nothing: %v", matchers)
	}
	var ranked []matcherPosting
	if err = store.file.View(t.Context(), func(tx *sql.Tx) error {
		var possible bool
		ranked, possible, err = rankMatchers(t.Context(), tx, matchers)
		if !possible && err == nil {
			t.Fatal("both matchers name series, so the match must be possible")
		}
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if len(ranked) != 2 {
		t.Fatalf("ranked %d matchers, want 2", len(ranked))
	}
	if ranked[0].seriesCount != 1 || ranked[1].seriesCount != crowd {
		t.Fatalf("drove from a list of %d with %d behind it, want 1 then %d", ranked[0].seriesCount, ranked[1].seriesCount, crowd)
	}
}

func TestAMatcherNamingNothingEndsTheMatch(t *testing.T) {
	store, _ := openTestStore(t, Options{Retention: time.Hour})
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: testSamples(4)}}); err != nil {
		t.Fatal(err)
	}
	result, err := store.Read(t.Context(), Range{
		Name: "cpu", Match: Labels{"host": "absent"},
		From: testEpoch, To: testEpoch + 4,
	})
	if err != nil {
		t.Fatal(err)
	}
	if len(result) != 0 {
		t.Fatalf("matched %d series on a label nobody carries", len(result))
	}
}

func TestPostingCountsRankAboveTheOldProbeCap(t *testing.T) {
	store, _ := openTestStore(t, Options{MaxSeries: 2048})
	batches := make([]Batch, 2048)
	for i := range batches {
		zone := "other"
		if i < 1025 {
			zone = "hot"
		}
		batches[i] = Batch{Series: Series{Name: "cpu", Labels: Labels{"host": fmt.Sprint(i), "zone": zone}}, Samples: testSamples(1)}
	}
	if err := store.Ingest(t.Context(), batches); err != nil {
		t.Fatal(err)
	}
	if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
		ranked, possible, err := rankMatchers(t.Context(), tx, []label{{Name: "__name__", Value: "cpu"}, {Name: "zone", Value: "hot"}})
		if err != nil {
			return err
		}
		if !possible || len(ranked) != 2 || ranked[0].seriesCount != 1025 || ranked[1].seriesCount != 2048 {
			t.Fatalf("ranked posting counts: %+v, possible %v", ranked, possible)
		}
		var name string
		if err = tx.QueryRowContext(t.Context(), `select name from label_values where id=?`, ranked[0].labelID).Scan(&name); err != nil {
			return err
		}
		if name != "zone" {
			t.Fatalf("drove from %s instead of the shorter zone posting", name)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	newSeries := Series{Name: "cpu", Labels: Labels{"host": "overflow"}}
	if err := store.Ingest(t.Context(), []Batch{{Series: newSeries, Samples: testSamples(1)}}); !errors.Is(err, ErrLimit) {
		t.Fatalf("cardinality rejection: %v", err)
	}
	if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
		var count int
		if err := tx.QueryRowContext(t.Context(), `select posting_count from label_values where name='__name__' and value='cpu'`).Scan(&count); err != nil {
			return err
		}
		if count != 2048 {
			t.Fatalf("failed registration changed posting count to %d", count)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func TestMatcherLookupBatchesOnlyLargeSelectors(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	labels := []label{{Name: "__name__", Value: "cpu"}}
	for i := range 7 {
		labels = append(labels, label{Name: fmt.Sprintf("dimension_%d", i), Value: fmt.Sprintf("value_%d", i)})
	}
	if err := store.Ingest(t.Context(), []Batch{{Series: publicSeries(labels, ""), Samples: testSamples(1)}}); err != nil {
		t.Fatal(err)
	}
	for _, test := range []struct {
		count, calls int
	}{
		{count: 2, calls: 2},
		{count: 8, calls: 1},
	} {
		if err := store.file.ViewPrepared(t.Context(), func(reader sqlite.Reader) error {
			counted := &countingReader{Reader: reader}
			ranked, possible, err := rankMatchers(t.Context(), counted, labels[:test.count])
			if err != nil {
				return err
			}
			if !possible || len(ranked) != test.count || counted.calls != test.calls {
				t.Fatalf("%d matchers: %d ranked, %d SQL calls", test.count, len(ranked), counted.calls)
			}
			return nil
		}); err != nil {
			t.Fatal(err)
		}
	}
}

func BenchmarkRankThirtyTwoMatchers(b *testing.B) {
	store, err := openAt(b, filepath.Join(b.TempDir(), fileName), Options{})
	if err != nil {
		b.Fatal(err)
	}
	labels := []label{{Name: "__name__", Value: "cpu"}}
	for i := range 31 {
		labels = append(labels, label{Name: fmt.Sprintf("dimension_%d", i), Value: fmt.Sprintf("value_%d", i)})
	}
	if err := store.Ingest(b.Context(), []Batch{{Series: publicSeries(labels, ""), Samples: []Sample{{At: time.Now().UnixMilli(), Value: 1}}}}); err != nil {
		b.Fatal(err)
	}
	b.ReportAllocs()
	b.ResetTimer()
	for range b.N {
		if err := store.file.ViewPrepared(b.Context(), func(reader sqlite.Reader) error {
			ranked, possible, err := rankMatchers(b.Context(), reader, labels)
			if err != nil {
				return err
			}
			if !possible || len(ranked) != len(labels) {
				b.Fatal("lost a matcher")
			}
			return nil
		}); err != nil {
			b.Fatal(err)
		}
	}
}
