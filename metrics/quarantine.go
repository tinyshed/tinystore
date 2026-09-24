package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"strings"
)

const suspendQuery = `update series_state set failed_at=?,failure_reason=? where series_id=? and failed_at is null`

// handleMaintenanceFailure suspends a series whose maintenance failed on its
// own corrupt data or limits, and reports whether this call suspended it; any
// other failure is returned for the pass to stop on.
func (s *Store) handleMaintenanceFailure(ctx context.Context, id int64, phase string, cause error) (bool, error) {
	if ctx.Err() != nil {
		return false, ctx.Err()
	}
	isolated := errors.Is(cause, ErrCorrupt) || errors.Is(cause, ErrLimit)
	if !isolated {
		return false, cause
	}
	reason := maintenanceFailureReason(phase, cause)
	var changed int64
	err := s.file.Update(ctx, func(tx *sql.Tx) error {
		result, err := tx.ExecContext(ctx, suspendQuery, s.now().UnixMilli(), reason, id)
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

const retryQuery = `
	update series_state set failed_at=null,failure_reason=null
	where series_id in (
		select series_id from series_state where failed_at is not null
		order by failed_at,series_id limit cast(? as integer)
	)`

// RetryFailedMaintenance re-enables at most MaintenanceSeries suspended series per call.
func (s *Store) RetryFailedMaintenance(ctx context.Context) (int, error) {
	if err := s.enter(ctx); err != nil {
		return 0, err
	}
	defer s.leave()

	release, err := s.holdMaintenance(ctx)
	if err != nil {
		return 0, err
	}
	defer release()

	changed, err := s.clearFailures(ctx)
	if err != nil {
		return 0, err
	}
	s.quarantined.Add(-changed)
	return int(changed), nil
}

func (s *Store) clearFailures(ctx context.Context) (int64, error) {
	var changed int64
	err := s.file.Update(ctx, func(tx *sql.Tx) error {
		result, err := tx.ExecContext(ctx, retryQuery, s.opts.MaintenanceSeries)
		if err != nil {
			return fmt.Errorf("retry suspended maintenance: %w", err)
		}
		changed, err = result.RowsAffected()
		return err
	})
	return changed, err
}

const failuresQuery = `
	select series_id,failed_at,failure_reason from series_state
	where failed_at is not null and series_id>?
	order by series_id limit cast(? as integer)`

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
		rows, err := tx.QueryContext(ctx, failuresQuery, afterID, s.opts.MaintenanceSeries)
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
