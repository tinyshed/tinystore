package records

import (
	"database/sql"
	"math"
	"math/rand/v2"
	"strings"
	"testing"
	"time"
)

// the examples in the comment on spanOf, and the ends of time
func TestABlockIsFiledByItsWidth(t *testing.T) {
	for _, test := range []struct {
		width time.Duration
		span  int
	}{{0, 0}, {time.Second / 2, 29}, {time.Hour, 42}, {14 * 24 * time.Hour, 51}} {
		if span := spanOf(fixtureBase, fixtureBase+int64(test.width)); span != test.span {
			t.Errorf("%s wide: span %d, want %d", test.width, span, test.span)
		}
	}
	if span := spanOf(math.MinInt64, math.MaxInt64); span != 63 {
		t.Errorf("the whole of time: span %d", span)
	}
	if end := latestEnd(63, 0); end != math.MaxInt64 {
		t.Errorf("the widest span ends at %d", end)
	}
	if end := latestEnd(10, math.MaxInt64-5); end != math.MaxInt64 {
		t.Errorf("a span past the end of time ends at %d", end)
	}
}

// the time index is walked within one span at a time, between a read's start
// and its end moved on by the span's width, never to the end of the index
func TestTheTimeIndexIsWalkedWithinEachSpan(t *testing.T) {
	s := openRecords(t)
	var plan []string
	err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		rows, err := tx.QueryContext(t.Context(), `explain query plan `+selectBlockCandidates, 1, 2, 3, 4, 5, 6)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var id, parent, unused int
			var detail string
			if err = rows.Scan(&id, &parent, &unused, &detail); err != nil {
				return err
			}
			plan = append(plan, detail)
		}
		return rows.Err()
	})
	if err != nil {
		t.Fatal(err)
	}
	if want := "blocks_by_time (span=? AND last_at>? AND last_at<?)"; !strings.Contains(strings.Join(plan, " "), want) {
		t.Fatalf("plan %q, want %q", plan, want)
	}
}

// a read finds every record its range holds whatever the widths of the blocks
// holding them: a busy stream's, a quiet stream's merged over hours, and late
// ones, in the head and sealed
func TestReadsFindRecordsInBlocksOfEveryWidth(t *testing.T) {
	s := openRecords(t)
	all := frontendRecords(20_000)
	all[100].At = all[100].At.Add(-50 * time.Minute)
	s.append(t, all...)
	s.maintain(t)
	for range 9 {
		all = append(all, s.hourly(t, "quiet", 20)...)
		s.clock.advance(time.Hour)
		s.maintain(t)
	}
	all = append(all, s.hourly(t, "quiet", 5)...)

	random := rand.New(rand.NewPCG(3, 4))
	earliest, latest := testNow.Add(-2*time.Hour), s.clock.Now()
	for range 100 {
		from := earliest.Add(time.Duration(random.Int64N(int64(latest.Sub(earliest)))))
		to := from.Add(time.Duration(math.Pow(10, 3+random.Float64()*10)))
		want := map[string]int{}
		for _, record := range all {
			if !record.At.Before(from) && record.At.Before(to) {
				want[recordKey(&record)]++
			}
		}
		got := s.readAll(t, Query{From: from, To: to, Limit: maxLimit})
		for _, record := range got {
			want[recordKey(&record)]--
		}
		for key, missing := range want {
			if missing != 0 {
				t.Fatalf("[%s, %s): %s read %d times too few", from, to, key, missing)
			}
		}
	}
}

func recordKey(r *Record) string {
	return r.Stream + " " + r.At.String() + " " + r.Attrs[0].Value + r.Attrs[len(r.Attrs)-1].Value
}
