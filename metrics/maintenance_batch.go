package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
)

const (
	publicationBatchBytes  = 1 << 20
	publicationBatchSeries = 8
)

type stagedPublication struct {
	candidate packingCandidate
	group     blockGroup
}

func publicationBytes(candidate packingCandidate, group blockGroup) int {
	bytes := len(candidate.points)*16 + len(group.clockBody)
	for _, block := range group.blocks {
		bytes += len(block.clock) + len(block.body)
	}
	return bytes
}

func (s *Store) publishBatch(ctx context.Context, staged []stagedPublication, cutoff int64) (Maintenance, error) {
	var committed Maintenance
	if len(staged) == 0 {
		return committed, nil
	}
	var pending Maintenance
	err := s.file.Update(ctx, func(tx *sql.Tx) error {
		for _, item := range staged {
			if _, err := tx.ExecContext(ctx, `savepoint maintenance_series`); err != nil {
				return fmt.Errorf("start series publication: %w", err)
			}
			publishErr := s.publishTx(ctx, tx, item.candidate, item.group, cutoff)
			if publishErr != nil {
				_, rollbackErr := tx.ExecContext(ctx, `rollback to maintenance_series`)
				_, releaseErr := tx.ExecContext(ctx, `release maintenance_series`)
				if rollbackErr != nil || releaseErr != nil {
					return errors.Join(publishErr, rollbackErr, releaseErr)
				}
				switch {
				case errors.Is(publishErr, ErrConflict):
					pending.Conflicts++
					continue
				case errors.Is(publishErr, ErrCorrupt):
					reason := maintenanceFailureReason("publish block", publishErr)
					result, err := tx.ExecContext(ctx, `update series_state set failed_at=?,failure_reason=? where series_id=? and failed_at is null`, s.now().UnixMilli(), reason, item.candidate.seriesID)
					if err != nil {
						return fmt.Errorf("suspend failed publication: %w", err)
					}
					changed, err := result.RowsAffected()
					if err != nil {
						return fmt.Errorf("count suspended publication: %w", err)
					}
					pending.QuarantinedSeries += int(changed)
					continue
				default:
					return publishErr
				}
			}
			if _, err := tx.ExecContext(ctx, `release maintenance_series`); err != nil {
				return fmt.Errorf("finish series publication: %w", err)
			}
			pending.SealedBlocks += len(item.group.blocks)
		}
		return nil
	})
	if err != nil {
		return committed, err
	}
	committed = pending
	return committed, nil
}
