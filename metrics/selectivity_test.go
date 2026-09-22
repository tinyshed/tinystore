package metrics

import (
	"database/sql"
	"fmt"
	"testing"
	"time"
)

// the driving matcher decides how many posting rows the match scans, and a
// matcher ordered by label name drove from `__name__` at any cardinality
func TestTheShortestPostingListDrivesTheMatch(t *testing.T) {
	store, _ := openTestStore(t, Options{Retention: time.Hour})
	const crowd = 200
	for i := range crowd {
		series := Series{Labels: []Label{{Name: "__name__", Value: "cpu"}, {Name: "host", Value: fmt.Sprintf("host_%d", i)}}, Kind: Gauge}
		if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: []Sample{{At: testEpoch, Value: 1}}}}); err != nil {
			t.Fatal(err)
		}
	}
	matchers, _, err := canonicalLabels([]Label{{Name: "__name__", Value: "cpu"}, {Name: "host", Value: "host_7"}}, false)
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
	if ranked[0].names != 1 || ranked[1].names != crowd {
		t.Fatalf("drove from a list of %d with %d behind it, want 1 then %d", ranked[0].names, ranked[1].names, crowd)
	}
}

func TestAMatcherNamingNothingEndsTheMatch(t *testing.T) {
	store, _ := openTestStore(t, Options{Retention: time.Hour})
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: testSamples(4)}}); err != nil {
		t.Fatal(err)
	}
	result, err := store.Read(t.Context(), Range{
		Matchers: []Label{{Name: "__name__", Value: "cpu"}, {Name: "host", Value: "absent"}},
		From:     testEpoch, To: testEpoch + 4,
	})
	if err != nil {
		t.Fatal(err)
	}
	if len(result) != 0 {
		t.Fatalf("matched %d series on a label nobody carries", len(result))
	}
}
