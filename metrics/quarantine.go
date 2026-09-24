package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"strings"
)

func (s *Store) handleMaintenanceFailure(ctx context.Context, id int64, phase string, cause error, allowLimit bool) (bool, error) {
	if ctx.Err() != nil {
		return false, ctx.Err()
	}
	isolated := errors.Is(cause, ErrCorrupt) || allowLimit && errors.Is(cause, ErrLimit)
	if !isolated {
		return false, cause
	}
	reason := maintenanceFailureReason(phase, cause)
	var changed int64
	err := s.file.Update(ctx, func(tx *sql.Tx) error {
		result, err := tx.ExecContext(ctx, `update series_state set failed_at=?,failure_reason=? where series_id=? and failed_at is null`, s.now().UnixMilli(), reason, id)
		if err != nil {
			return fmt.Errorf("suspend series maintenance: %w", err)
		}
		changed, err = result.RowsAffected()
		return err
	})
	if err != nil {
		return false, errors.Join(cause, err)
	}
	if changed == 1 {
		s.quarantined.Add(1)
	}
	return changed == 1, nil
}

func maintenanceFailureReason(phase string, cause error) string {
	reason := fmt.Sprintf("%s: %v", phase, cause)
	if len(reason) > 1024 {
		return strings.ToValidUTF8(reason[:1024], "")
	}
	return reason
}

// RetryFailedMaintenance re-enables at most MaintenanceSeries suspended series per call.
func (s *Store) RetryFailedMaintenance(ctx context.Context) (int, error) {
	if err := s.enter(ctx); err != nil {
		return 0, err
	}
	defer s.leave()
	select {
	case <-s.maintenanceGate:
	case <-ctx.Done():
		return 0, ctx.Err()
	}
	defer func() { s.maintenanceGate <- struct{}{} }()
	var changed int64
	err := s.file.Update(ctx, func(tx *sql.Tx) error {
		result, err := tx.ExecContext(ctx, `update series_state set failed_at=null,failure_reason=null where series_id in (select series_id from series_state where failed_at is not null order by failed_at,series_id limit cast(? as integer))`, s.opts.MaintenanceSeries)
		if err != nil {
			return fmt.Errorf("retry suspended maintenance: %w", err)
		}
		changed, err = result.RowsAffected()
		return err
	})
	if err != nil {
		return 0, err
	}
	s.quarantined.Add(-changed)
	return int(changed), nil
}

// ListMaintenanceFailures returns one bounded page ordered by file-local series id.
func (s *Store) ListMaintenanceFailures(ctx context.Context, afterID int64) ([]MaintenanceFailure, error) {
	if afterID < 0 {
		return nil, fmt.Errorf("%w: maintenance failure cursor", ErrInvalid)
	}
	if err := s.enter(ctx); err != nil {
		return nil, err
	}
	defer s.leave()
	ctx, cancel := context.WithTimeout(ctx, s.opts.SnapshotTimeout)
	defer cancel()
	var failures []MaintenanceFailure
	err := s.file.View(ctx, func(tx *sql.Tx) error {
		rows, err := tx.QueryContext(ctx, `select series_id,failed_at,failure_reason from series_state where failed_at is not null and series_id>? order by series_id limit cast(? as integer)`, afterID, s.opts.MaintenanceSeries)
		if err != nil {
			return fmt.Errorf("list suspended maintenance: %w", err)
		}
		defer rows.Close()
		for rows.Next() {
			var failure MaintenanceFailure
			if err = rows.Scan(&failure.SeriesID, &failure.FailedAt, &failure.Reason); err != nil {
				return fmt.Errorf("read suspended maintenance: %w", err)
			}
			failures = append(failures, failure)
		}
		return rows.Err()
	})
	return failures, err
}
