package metrics

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"log/slog"
	"testing"
)

func TestAQuarantinedSeriesIsLoggedOnceAfterItsCommit(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	var output bytes.Buffer
	s.log = slog.New(slog.NewJSONHandler(&output, nil))
	series := testSeries()
	if err := s.Ingest(t.Context(), []Batch{{Series: series, Samples: testSamples(241)}}); err != nil {
		t.Fatal(err)
	}
	ready, err := s.readyToSeal(t.Context())
	if err != nil || len(ready) != 1 {
		t.Fatalf("ready series: %v, %v", ready, err)
	}
	id := ready[0]
	err = s.file.Update(t.Context(), func(tx *sql.Tx) error {
		var tail []byte
		if scanErr := tx.QueryRowContext(t.Context(), `select tail from series_state where series_id=?`, id).Scan(&tail); scanErr != nil {
			return scanErr
		}
		tail[len(tail)-1] ^= 0xff
		_, updateErr := tx.ExecContext(t.Context(), `update series_state set tail=? where series_id=?`, tail, id)
		return updateErr
	})
	if err != nil {
		t.Fatal(err)
	}
	result, err := s.Maintain(t.Context())
	if err != nil || result.QuarantinedSeries != 1 {
		t.Fatalf("quarantine: %+v, %v", result, err)
	}
	if _, err = s.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	lines := bytes.Split(bytes.TrimSpace(output.Bytes()), []byte("\n"))
	if len(lines) != 1 {
		t.Fatalf("logged %d events: %s", len(lines), output.String())
	}
	var event map[string]any
	if err = json.Unmarshal(lines[0], &event); err != nil {
		t.Fatal(err)
	}
	if event["msg"] != "series suspended" || event["series_id"] != float64(id) ||
		event["phase"] != "read head" || event["reason"] == "" {
		t.Fatalf("quarantine event: %v", event)
	}
	if bytes.Contains(lines[0], []byte("host=one")) {
		t.Fatalf("label value leaked into default event: %s", lines[0])
	}
	if err = s.runtime.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
}

func TestBackgroundMaintenanceLogsOneDebugSummary(t *testing.T) {
	s, _ := openTestStore(t, Options{})
	var output bytes.Buffer
	s.log = slog.New(slog.NewJSONHandler(&output, &slog.HandlerOptions{Level: slog.LevelDebug}))
	if err := s.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: testSamples(241)}}); err != nil {
		t.Fatal(err)
	}
	if err := s.maintainInBackground(t.Context()); err != nil {
		t.Fatal(err)
	}
	lines := bytes.Split(bytes.TrimSpace(output.Bytes()), []byte("\n"))
	if len(lines) != 1 {
		t.Fatalf("maintenance logged %d lines: %s", len(lines), output.String())
	}
	var event map[string]any
	if err := json.Unmarshal(lines[0], &event); err != nil {
		t.Fatal(err)
	}
	if event["msg"] != "maintenance finished" || event["sealed_blocks"] != float64(1) ||
		event["failed"] != false || event["duration"] == nil {
		t.Fatalf("maintenance summary: %v", event)
	}
}
