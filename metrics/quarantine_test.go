package metrics

import (
	"context"
	"database/sql"
	"errors"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestCorruptSeriesDoesNotStopOtherMaintenance(t *testing.T) {
	s, path := openTestStore(t, Options{MaintenanceSeries: 2})
	a := Series{Labels: []Label{{Name: "__name__", Value: "a"}}}
	b := Series{Labels: []Label{{Name: "__name__", Value: "b"}}}
	points := testSamples(241)
	if err := s.Ingest(t.Context(), []Batch{{Series: a, Samples: points}, {Series: b, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	var failedID int64
	if err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
		if err := tx.QueryRowContext(t.Context(), `select p.series_id from postings p join label_values v on v.id=p.label_id where v.name='__name__' and v.value='a'`).Scan(&failedID); err != nil {
			return err
		}
		var tail []byte
		if err := tx.QueryRowContext(t.Context(), `select tail from series_state where series_id=?`, failedID).Scan(&tail); err != nil {
			return err
		}
		tail[len(tail)-1] ^= 0xff
		_, err := tx.ExecContext(t.Context(), `update series_state set tail=? where series_id=?`, tail, failedID)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	result, err := s.Maintain(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if result.QuarantinedSeries != 1 || result.SealedBlocks != 1 || s.Stats().QuarantinedSeries != 1 {
		t.Fatalf("maintenance result %+v, stats %+v", result, s.Stats())
	}
	if err = s.Ingest(t.Context(), []Batch{{Series: a, Samples: []Sample{{At: testEpoch + 241, Value: 1}}}}); !errors.Is(err, ErrSuspended) {
		t.Fatalf("ingest into suspended series: %v", err)
	}
	failures, err := s.ListMaintenanceFailures(t.Context(), 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(failures) != 1 || failures[0].SeriesID != failedID || !strings.Contains(failures[0].Reason, "checksum") {
		t.Fatalf("persisted failures: %+v", failures)
	}
	if err = s.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	reopened, err := Open(t.Context(), path, Options{MaintenanceSeries: 2})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = reopened.Close(context.Background()) })
	reopened.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
	if reopened.Stats().QuarantinedSeries != 1 {
		t.Fatalf("lost suspended state on reopen: %+v", reopened.Stats())
	}
	retried, err := reopened.RetryFailedMaintenance(t.Context())
	if err != nil || retried != 1 || reopened.Stats().QuarantinedSeries != 0 {
		t.Fatalf("retry: %d, %v, %+v", retried, err, reopened.Stats())
	}
	result, err = reopened.Maintain(t.Context())
	if err != nil || result.QuarantinedSeries != 1 {
		t.Fatalf("damaged head after retry: %+v, %v", result, err)
	}
}

func TestSuspendedLimitCanRecoverAfterReopen(t *testing.T) {
	path := filepath.Join(t.TempDir(), "limits.db")
	series := Series{Labels: []Label{{Name: "__name__", Value: "limited"}}}
	open := func(maxHeadSamples int) *Store {
		t.Helper()
		s, err := Open(t.Context(), path, Options{MaxHeadSamples: maxHeadSamples, MaintenanceSeries: 1})
		if err != nil {
			t.Fatal(err)
		}
		s.now = func() time.Time { return time.UnixMilli(testEpoch + 900) }
		return s
	}
	s := open(512)
	if err := s.Ingest(t.Context(), []Batch{{Series: series, Samples: testSamples(241)}}); err != nil {
		t.Fatal(err)
	}
	if err := s.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	s = open(240)
	result, err := s.Maintain(t.Context())
	if err != nil || result.QuarantinedSeries != 1 || s.Stats().QuarantinedSeries != 1 {
		t.Fatalf("lowered limit: %+v, %v, %+v", result, err, s.Stats())
	}
	if err = s.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	s = open(512)
	t.Cleanup(func() { _ = s.Close(context.Background()) })
	if s.Stats().QuarantinedSeries != 1 {
		t.Fatalf("lost suspended limit: %+v", s.Stats())
	}
	retried, err := s.RetryFailedMaintenance(t.Context())
	if err != nil || retried != 1 {
		t.Fatalf("retry raised limit: %d, %v", retried, err)
	}
	result, err = s.Maintain(t.Context())
	if err != nil || result.SealedBlocks != 1 || s.Stats().QuarantinedSeries != 0 {
		t.Fatalf("maintenance after raising limit: %+v, %v, %+v", result, err, s.Stats())
	}
}

func TestMaintenanceFailurePagesAndRetryStayBounded(t *testing.T) {
	s, path := openTestStore(t, Options{MaintenanceSeries: 1})
	a := Series{Labels: []Label{{Name: "__name__", Value: "a"}}}
	b := Series{Labels: []Label{{Name: "__name__", Value: "b"}}}
	if err := s.Ingest(t.Context(), []Batch{{Series: a, Samples: testSamples(1)}, {Series: b, Samples: testSamples(1)}}); err != nil {
		t.Fatal(err)
	}
	if err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), `update series_state set failed_at=?,failure_reason='test failure'`, testEpoch)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if err := s.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	s, err := Open(t.Context(), path, Options{MaintenanceSeries: 1})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = s.Close(context.Background()) })
	if got := s.Stats().QuarantinedSeries; got != 2 {
		t.Fatalf("reopened failure count %d", got)
	}
	first, err := s.ListMaintenanceFailures(t.Context(), 0)
	if err != nil || len(first) != 1 {
		t.Fatalf("first failure page: %+v, %v", first, err)
	}
	second, err := s.ListMaintenanceFailures(t.Context(), first[0].SeriesID)
	if err != nil || len(second) != 1 || second[0].SeriesID <= first[0].SeriesID {
		t.Fatalf("second failure page: %+v, %v", second, err)
	}
	for want := uint64(1); ; want-- {
		changed, retryErr := s.RetryFailedMaintenance(t.Context())
		if retryErr != nil || changed != 1 || s.Stats().QuarantinedSeries != want {
			t.Fatalf("bounded retry: %d, %v, %+v", changed, retryErr, s.Stats())
		}
		if want == 0 {
			break
		}
	}
}
