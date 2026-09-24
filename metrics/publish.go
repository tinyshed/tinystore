package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"math"
)

func (s *Store) publish(ctx context.Context, candidate packingCandidate, group blockGroup, cutoff int64) error {
	return s.file.Update(ctx, func(tx *sql.Tx) error {
		return s.publishTx(ctx, tx, candidate, group, cutoff)
	})
}

func (s *Store) publishTx(ctx context.Context, tx *sql.Tx, candidate packingCandidate, group blockGroup, cutoff int64) error {
	var version int64
	if err := tx.QueryRowContext(ctx, `select version from series_state where series_id=?`, candidate.seriesID).Scan(&version); err != nil {
		return fmt.Errorf("check packing version: %w", err)
	}
	if version != candidate.version {
		return ErrConflict
	}
	if version == math.MaxInt64 {
		return fmt.Errorf("%w: series version exhausted", ErrLimit)
	}
	merged, replaced, mergeErr := s.mergePrecedingGroups(ctx, tx, group)
	if mergeErr != nil {
		return mergeErr
	}
	group = merged
	clockID, clockErr := acquireClock(ctx, tx, group.clockBody)
	if clockErr != nil {
		return clockErr
	}
	group.clockID = clockID
	allocated := int64(0)
	for slot, block := range group.blocks {
		if group.isExternal(slot) && block.payload == 0 {
			allocated++
		}
	}
	var nextPayload int64
	if allocated > 0 {
		if err := tx.QueryRowContext(ctx, `select next_payload_id from store_state where id=1`).Scan(&nextPayload); err != nil {
			return fmt.Errorf("read payload allocation: %w", err)
		}
		if nextPayload > math.MaxInt64-allocated {
			return fmt.Errorf("%w: payload identifiers exhausted", ErrLimit)
		}
		if _, err := tx.ExecContext(ctx, `update store_state set next_payload_id=? where id=1`, nextPayload+allocated); err != nil {
			return fmt.Errorf("reserve payload identifiers: %w", err)
		}
		if group.format == 2 {
			group.firstPayload = nextPayload
		}
	}
	for slot := range group.blocks {
		block := &group.blocks[slot]
		if group.isExternal(slot) && block.payload == 0 {
			block.payload = nextPayload
			nextPayload++
			if _, err := tx.ExecContext(ctx, `insert into payloads values(?,?)`, block.payload, block.body); err != nil {
				return fmt.Errorf("write sealed payload: %w", err)
			}
		}
	}
	directory, err := s.writeDirectory(group)
	if err != nil {
		return err
	}
	if err = removeMergedGroups(ctx, tx, replaced); err != nil {
		return err
	}
	if _, err = tx.ExecContext(ctx, `insert into groups values(?,?,?,?,?)`, group.seriesID, group.start, group.end, directory, group.clockID); err != nil {
		return fmt.Errorf("publish group directory: %w", err)
	}
	existing, err := s.mutablePoints(ctx, tx, group.seriesID)
	if err != nil {
		return err
	}
	remaining, err := removeSealed(existing, candidate.points)
	if err != nil {
		return err
	}
	if err = s.saveHead(ctx, tx, group.seriesID, remaining); err != nil {
		return err
	}
	ready := 0
	if s.headReady(remaining, candidate.maxSeen, cutoff) {
		ready = 1
	}
	if _, err = tx.ExecContext(ctx, `update series_state set sealed_before=?,version=version+1,ready=?,model_scale=? where series_id=?`, group.end+1, ready, group.modelScale, group.seriesID); err != nil {
		return fmt.Errorf("advance sealed frontier: %w", err)
	}
	return s.refreshDue(ctx, tx, group.seriesID)
}

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
