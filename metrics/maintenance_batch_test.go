package metrics

import (
	"database/sql"
	"testing"
)

func stagedTestGroups(t *testing.T, s *Store) ([]stagedPublication, []int64) {
	t.Helper()
	a := Series{Labels: []Label{{Name: "__name__", Value: "a"}}}
	b := Series{Labels: []Label{{Name: "__name__", Value: "b"}}}
	if err := s.Ingest(t.Context(), []Batch{{Series: a, Samples: testSamples(241)}, {Series: b, Samples: testSamples(241)}}); err != nil {
		t.Fatal(err)
	}
	ids, err := s.dueSeries(t.Context(), true, s.cutoff())
	if err != nil || len(ids) != 2 {
		t.Fatalf("ready ids: %v, %v", ids, err)
	}
	staged := make([]stagedPublication, len(ids))
	for i, id := range ids {
		candidate, err := s.readCandidate(t.Context(), id, s.cutoff())
		if err != nil {
			t.Fatal(err)
		}
		group, err := s.encodeCandidate(t.Context(), candidate)
		if err != nil {
			t.Fatal(err)
		}
		candidate.points = candidate.points[:blockSamples]
		staged[i] = stagedPublication{candidate: candidate, group: group}
	}
	return staged, ids
}

func TestPublicationBatchRollsBackOneConflictingSeries(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	staged, ids := stagedTestGroups(t, s)
	b := Series{Labels: []Label{{Name: "__name__", Value: "b"}}}
	if err := s.Ingest(t.Context(), []Batch{{Series: b, Samples: []Sample{{At: testEpoch + 1, Value: 99}}}}); err != nil {
		t.Fatal(err)
	}
	result, err := s.publishBatch(t.Context(), staged, s.cutoff())
	if err != nil || result.SealedBlocks != 1 || result.Conflicts != 1 {
		t.Fatalf("publication result: %+v, %v", result, err)
	}
	if err = s.file.View(t.Context(), func(tx *sql.Tx) error {
		for i, id := range ids {
			var groups int
			if readErr := tx.QueryRowContext(t.Context(), `select count(*) from groups where series_id=?`, id).Scan(&groups); readErr != nil {
				return readErr
			}
			if groups != 1-i {
				t.Fatalf("series %d has %d groups", id, groups)
			}
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	resultForB, err := s.Read(t.Context(), Range{Matchers: b.Labels, From: testEpoch, To: testEpoch + 241})
	if err != nil || len(resultForB) != 1 || len(resultForB[0].Samples) != 241 || resultForB[0].Samples[1].Value != 99 {
		t.Fatalf("conflicting series changed: %+v, %v", resultForB, err)
	}
}

func TestPublicationBatchDoesNotCountRolledBackTransaction(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	staged, ids := stagedTestGroups(t, s)
	if err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), `create trigger reject_second before insert on groups when new.series_id=2 begin select raise(abort,'reject second'); end`)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	result, err := s.publishBatch(t.Context(), staged, s.cutoff())
	if err == nil || result.SealedBlocks != 0 {
		t.Fatalf("failed batch counted publication: %+v, %v", result, err)
	}
	if err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		for _, id := range ids {
			var groups int
			if err := tx.QueryRowContext(t.Context(), `select count(*) from groups where series_id=?`, id).Scan(&groups); err != nil {
				return err
			}
			if groups != 0 {
				t.Fatalf("series %d published through failed batch", id)
			}
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}
