package metrics

import (
	"database/sql"
	"testing"
)

func TestExistingSeriesIngestAvoidsUnchangedDueIndexes(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	state := ingestState{
		maxSeen: testEpoch,
		nextGC:  sql.NullInt64{Int64: testEpoch, Valid: true},
	}
	points := testSamples(241)
	check := func(merged []Sample, wantIndexWork bool) {
		t.Helper()
		incoming := merged[len(merged)-1:]
		next := headUpdate{
			count: len(merged),
			first: merged[0].At,
			last:  merged[len(merged)-1].At,
			ready: s.headReady(merged, max(state.maxSeen, incoming[0].At), testEpoch),
		}
		query, arguments := ingestUpdate(headWrite{id: 1, state: state, incoming: incoming}, next)
		indexWork := 0
		err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
			rows, err := tx.QueryContext(t.Context(), `explain `+query, arguments...)
			if err != nil {
				return err
			}
			defer rows.Close()
			for rows.Next() {
				var address, p1, p2, p3, p5 int
				var opcode string
				var p4, comment sql.NullString
				if err := rows.Scan(&address, &opcode, &p1, &p2, &p3, &p4, &p5, &comment); err != nil {
					return err
				}
				if opcode == "IdxDelete" || opcode == "IdxInsert" {
					indexWork++
				}
			}
			return rows.Err()
		})
		if err != nil {
			t.Fatal(err)
		}
		if (indexWork > 0) != wantIndexWork {
			t.Fatalf("index work %d for %d mutable points", indexWork, len(merged))
		}
		t.Logf("%d mutable points: %d index opcodes", len(merged), indexWork)
	}
	check(points[:1], false)
	check(points, true)
}
