package metrics

import (
	"database/sql"
	"errors"
	"fmt"
	"path/filepath"
	"testing"
	"time"
)

func TestBatchedReadsSpendNothingOnExpiredSeries(t *testing.T) {
	for _, count := range []int{15, 16} {
		for _, sealed := range []bool{false, true} {
			t.Run(fmt.Sprintf("%d/sealed=%v", count, sealed), func(t *testing.T) {
				now := testEpoch + time.Hour.Milliseconds()
				store := keptStore(t, &now, map[string]time.Duration{"audit_": 3 * time.Hour})
				var batches []Batch
				for i := range count {
					points := []Sample{{At: now, Value: 1}}
					if sealed {
						points = make([]Sample, 241)
						for j := range points {
							points[j] = Sample{At: now - 1000 + int64(j), Value: float64(j)}
						}
					}
					batches = append(batches, Batch{
						Series: Series{Name: "cpu", Labels: Labels{"host": fmt.Sprint(i)}}, Samples: points,
					})
				}
				if err := store.Ingest(t.Context(), batches); err != nil {
					t.Fatal(err)
				}
				if sealed {
					if _, err := store.Maintain(t.Context()); err != nil {
						t.Fatal(err)
					}
				}
				now += 2 * time.Hour.Milliseconds()
				request := Range{
					Name: "cpu", Since: 4 * time.Hour,
					Limits: Limits{DecodedSamples: 1, PayloadBytes: 1024},
				}
				if got, err := store.Read(t.Context(), request); err != nil || len(got) != 0 {
					t.Fatalf("expired Read: %v: %v", got, err)
				}
				if got, err := store.Latest(t.Context(), request); err != nil || len(got) != 0 {
					t.Fatalf("expired Latest: %v: %v", got, err)
				}
				got, err := store.Aggregate(t.Context(), AggregateRequest{
					Range: request, Width: time.Hour, Op: AggregateSum,
				})
				if err != nil || len(got) != 0 {
					t.Fatalf("expired Aggregate: %v: %v", got, err)
				}
			})
		}
	}
}

func TestARetentionPrefixReadsOnlyItsNames(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	for _, name := range []string{"api_requests", "cpu", "disk", "z_last", "ÿ_requests"} {
		if err := store.Ingest(t.Context(), []Batch{{
			Series: Series{Name: name}, Samples: []Sample{{At: testEpoch, Value: 1}},
		}}); err != nil {
			t.Fatal(err)
		}
	}
	for prefix, want := range map[string]string{"api_": "api_requests", "ÿ": "ÿ_requests"} {
		var found []string
		err := store.file.View(t.Context(), func(tx *sql.Tx) error {
			rows, err := tx.QueryContext(t.Context(), namesFromQuery, prefix, prefixEnd(prefix))
			if err != nil {
				return err
			}
			defer rows.Close()
			for rows.Next() {
				var id int64
				var name string
				if err := rows.Scan(&id, &name); err != nil {
					return err
				}
				found = append(found, name)
			}
			return rows.Err()
		})
		if err != nil || len(found) != 1 || found[0] != want {
			t.Fatalf("%q found %v: %v", prefix, found, err)
		}
	}
}

func keptStore(t *testing.T, now *int64, rules map[string]time.Duration) *Store {
	t.Helper()
	store, _ := openTestStore(t, Options{Retention: time.Hour, RetentionOf: rules})
	store.now = func() time.Time { return time.UnixMilli(*now) }
	return store
}

// A series of a name RetentionOf keeps longer is read, aggregated and expired
// at its own cutoff, the longest prefix winning: audit_ keeps 3h, audit_short_
// 30m, and cpu Retention's 1h.
func TestASeriesKeepsItsOwnRetention(t *testing.T) {
	now := testEpoch + 4*time.Hour.Milliseconds()
	store := keptStore(t, &now, map[string]time.Duration{"audit_": 3 * time.Hour, "audit_short_": 30 * time.Minute})
	minutes := func(m int64) int64 { return now - m*time.Minute.Milliseconds() }
	if err := store.Ingest(t.Context(), []Batch{
		{Series: Series{Name: "audit_logins"}, Samples: []Sample{{At: minutes(150), Value: 1}, {At: minutes(10), Value: 2}}},
		{Series: Series{Name: "audit_short_x"}, Samples: []Sample{{At: minutes(20), Value: 3}}},
		{Series: Series{Name: "cpu"}, Samples: []Sample{{At: minutes(50), Value: 4}}},
	}); err != nil {
		t.Fatal(err)
	}
	read := func(name string) []Sample {
		t.Helper()
		results, err := store.Read(t.Context(), Range{Name: name, Since: 5 * time.Hour})
		if err != nil || len(results) > 1 {
			t.Fatalf("%s: %+v: %v", name, results, err)
		}
		if len(results) == 0 {
			return nil
		}
		return results[0].Samples
	}
	if got := read("audit_logins"); len(got) != 2 {
		t.Fatalf("audit_logins kept for three hours: %+v", got)
	}

	now += time.Hour.Milliseconds() // audit_logins' first is 3.5h old, cpu's 1h50m, audit_short_x's 1h20m
	if got := read("audit_logins"); len(got) != 1 || got[0].Value != 2 {
		t.Fatalf("audit_logins past three hours: %+v", got)
	}
	if got, other := read("cpu"), read("audit_short_x"); got != nil || other != nil {
		t.Fatalf("series past their retention read %+v and %+v", got, other)
	}
	sums, err := store.Aggregate(t.Context(), AggregateRequest{
		Range: Range{Name: "audit_logins", From: testEpoch, To: now}, Width: 5 * time.Hour, Op: AggregateSum,
	})
	if err != nil || len(sums) != 1 || sums[0].Buckets[0].Value != 2 || !sums[0].Buckets[0].Partial {
		t.Fatalf("audit_logins' sum, cut by its own retention: %+v: %v", sums, err)
	}

	done, err := store.Maintain(t.Context())
	if err != nil || done.ExpiredSamples != 3 || done.ReclaimedSeries != 2 {
		t.Fatalf("maintenance past each series' retention: %+v: %v", done, err)
	}
	if got := read("audit_logins"); len(got) != 1 {
		t.Fatalf("maintenance expired audit_logins at Retention: %+v", got)
	}
}

// Ingest takes a sample inside its own series' retention: one too old for cpu
// is not too old for audit_logins.
func TestASampleOlderThanItsSeriesRetentionIsRefused(t *testing.T) {
	now := testEpoch + 4*time.Hour.Milliseconds()
	store := keptStore(t, &now, map[string]time.Duration{"audit_": 3 * time.Hour})
	old := []Sample{{At: now - 90*time.Minute.Milliseconds(), Value: 1}}
	if err := store.Ingest(t.Context(), []Batch{{Series: Series{Name: "audit_logins"}, Samples: old}}); err != nil {
		t.Fatalf("a sample inside audit_'s three hours: %v", err)
	}
	if err := store.Ingest(t.Context(), []Batch{{Series: Series{Name: "cpu"}, Samples: old}}); !errors.Is(err, ErrTooOld) {
		t.Fatalf("a sample past cpu's hour: %v", err)
	}
}

// Opening with changed rules rewrites the keep of the series whose names start
// with a prefix that came, went or changed, and of no other; a changed
// Retention rewrites none, since a series it keeps stores no keep.
func TestAChangedRuleReachesTheSeriesOfItsPrefix(t *testing.T) {
	path := filepath.Join(t.TempDir(), fileName)
	open := func(retention time.Duration, rules map[string]time.Duration) *Store {
		t.Helper()
		store, err := openAt(t, path, Options{Retention: retention, RetentionOf: rules})
		if err != nil {
			t.Fatal(err)
		}
		store.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
		return store
	}
	store := open(time.Hour, map[string]time.Duration{"audit_": 3 * time.Hour, "jobs_": 2 * time.Hour})
	var batches []Batch
	for _, name := range []string{"audit_logins", "audit_signups", "jobs_done", "cpu"} {
		batches = append(batches, Batch{Series: Series{Name: name}, Samples: []Sample{{At: testEpoch, Value: 1}}})
	}
	if err := store.Ingest(t.Context(), batches); err != nil {
		t.Fatal(err)
	}
	hours := func(h int64) sql.NullInt64 { return sql.NullInt64{Int64: h * time.Hour.Milliseconds(), Valid: true} }
	checkKept(t, store, map[string]sql.NullInt64{
		"audit_logins": hours(3), "audit_signups": hours(3), "jobs_done": hours(2), "cpu": {},
	})

	reopen := func(retention time.Duration, rules map[string]time.Duration) {
		t.Helper()
		if err := store.runtime.Close(t.Context()); err != nil {
			t.Fatal(err)
		}
		store = open(retention, rules)
	}
	reopen(time.Hour, map[string]time.Duration{"audit_": 5 * time.Hour, "jobs_": 2 * time.Hour, "cp": time.Minute})
	checkKept(t, store, map[string]sql.NullInt64{
		"audit_logins": hours(5), "audit_signups": hours(5), "jobs_done": hours(2), "cpu": {Int64: 60_000, Valid: true},
	})
	reopen(24*time.Hour, map[string]time.Duration{"audit_": 5 * time.Hour})
	checkKept(t, store, map[string]sql.NullInt64{
		"audit_logins": hours(5), "audit_signups": hours(5), "jobs_done": {}, "cpu": {},
	})
}

const keptQuery = `
	select v.value, state.keep from series_state state
	join postings p on p.series_id = state.series_id
	join label_values v on v.id = p.label_id and v.name = '__name__'`

func checkKept(t *testing.T, store *Store, want map[string]sql.NullInt64) {
	t.Helper()
	got := map[string]sql.NullInt64{}
	err := store.file.View(t.Context(), func(tx *sql.Tx) error {
		rows, err := tx.QueryContext(t.Context(), keptQuery)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var name string
			var keep sql.NullInt64
			if err = rows.Scan(&name, &keep); err != nil {
				return err
			}
			got[name] = keep
		}
		return rows.Err()
	})
	if err != nil || len(got) != len(want) {
		t.Fatalf("kept %v, want %v: %v", got, want, err)
	}
	for name, keep := range want {
		if got[name] != keep {
			t.Fatalf("%s kept %v, want %v", name, got[name], keep)
		}
	}
}
