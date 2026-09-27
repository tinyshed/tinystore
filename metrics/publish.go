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

// publishTx writes one sealed group in the writer's transaction: it checks
// that the head has not changed since the group was encoded, absorbs preceding
// groups, stores payloads and the directory, then moves the frontier.
func (s *Store) publishTx(
	ctx context.Context, tx *sql.Tx, candidate packingCandidate, group blockGroup, cutoff int64,
) error {
	if err := checkPackingVersion(ctx, tx, candidate); err != nil {
		return err
	}

	group, replaced, err := s.mergePrecedingGroups(ctx, tx, group)
	if err != nil {
		return err
	}
	if group.clockID, err = acquireClock(ctx, tx, group.clockBody); err != nil {
		return err
	}

	if err = storePayloads(ctx, tx, &group); err != nil {
		return err
	}

	if err = s.replaceGroups(ctx, tx, group, replaced); err != nil {
		return err
	}

	return s.advanceFrontier(ctx, tx, candidate, group, cutoff)
}

const packingVersionQuery = `select version from series_state where series_id=?`

// checkPackingVersion refuses a group encoded from a head that changed since.
func checkPackingVersion(ctx context.Context, tx *sql.Tx, candidate packingCandidate) error {
	var version int64
	if err := tx.QueryRowContext(ctx, packingVersionQuery, candidate.seriesID).Scan(&version); err != nil {
		return fmt.Errorf("check packing version: %w", err)
	}
	if version != candidate.version {
		return ErrConflict
	}
	if version == math.MaxInt64 {
		return fmt.Errorf("%w: series version exhausted", ErrLimit)
	}
	return nil
}

const (
	nextPayloadQuery    = `select next_payload_id from store_state where id=1`
	reservePayloadQuery = `update store_state set next_payload_id=? where id=1`
	insertPayloadQuery  = `insert into payloads values(?,?)`
)

// storePayloads gives each new external body the next payload identifier and
// writes it; bodies a merge carried over keep theirs.
func storePayloads(ctx context.Context, tx *sql.Tx, group *blockGroup) error {
	allocated := int64(0)
	for slot, block := range group.blocks {
		if group.isExternal(slot) && block.payload == 0 {
			allocated++
		}
	}
	if allocated == 0 {
		return nil
	}

	var next int64
	if err := tx.QueryRowContext(ctx, nextPayloadQuery).Scan(&next); err != nil {
		return fmt.Errorf("read payload allocation: %w", err)
	}
	if next > math.MaxInt64-allocated {
		return fmt.Errorf("%w: payload identifiers exhausted", ErrLimit)
	}
	if _, err := tx.ExecContext(ctx, reservePayloadQuery, next+allocated); err != nil {
		return fmt.Errorf("reserve payload identifiers: %w", err)
	}
	if group.format == 2 {
		group.firstPayload = next
	}

	for slot := range group.blocks {
		block := &group.blocks[slot]
		if !group.isExternal(slot) || block.payload != 0 {
			continue
		}
		block.payload = next
		next++
		if _, err := tx.ExecContext(ctx, insertPayloadQuery, block.payload, block.body); err != nil {
			return fmt.Errorf("write sealed payload: %w", err)
		}
	}
	return nil
}

const insertGroupQuery = `insert into groups values(?,?,?,?,?)`

// replaceGroups writes the group's directory in place of the groups it absorbed.
func (s *Store) replaceGroups(ctx context.Context, tx *sql.Tx, group blockGroup, replaced []blockGroup) error {
	directory, err := s.writeDirectory(group)
	if err != nil {
		return err
	}
	if err = removeMergedGroups(ctx, tx, replaced); err != nil {
		return err
	}
	_, err = tx.ExecContext(ctx, insertGroupQuery, group.seriesID, group.start, group.end, directory, group.clockID)
	if err != nil {
		return fmt.Errorf("publish group directory: %w", err)
	}
	return nil
}

const advanceFrontierQuery = `
	update series_state set sealed_before=?,version=version+1,ready=?,model_scale=?
	where series_id=?`

// advanceFrontier removes the sealed samples from the head and moves the
// sealed frontier past the group, in the same transaction as the group.
func (s *Store) advanceFrontier(
	ctx context.Context, tx *sql.Tx, candidate packingCandidate, group blockGroup, cutoff int64,
) error {
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
	_, err = tx.ExecContext(ctx, advanceFrontierQuery, group.end+1, ready, group.modelScale, group.seriesID)
	if err != nil {
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

// publishBatch publishes staged groups in one transaction; the counts move
// only once it commits.
func (s *Store) publishBatch(ctx context.Context, staged []stagedPublication, cutoff int64) (Maintenance, error) {
	if len(staged) == 0 {
		return Maintenance{}, nil
	}
	var pending Maintenance
	var suspended []suspendedPublication
	err := s.file.Update(ctx, func(tx *sql.Tx) error {
		for _, item := range staged {
			outcome, reason, err := s.publishIsolated(ctx, tx, item, cutoff)
			if err != nil {
				return err
			}
			if outcome.QuarantinedSeries != 0 {
				suspended = append(suspended, suspendedPublication{seriesID: item.candidate.seriesID, reason: reason})
			}
			pending.SealedBlocks += outcome.SealedBlocks
			pending.Conflicts += outcome.Conflicts
			pending.QuarantinedSeries += outcome.QuarantinedSeries
		}
		return nil
	})
	if err != nil {
		return Maintenance{}, err
	}
	for _, event := range suspended {
		s.log.Warn("series suspended", "series_id", event.seriesID, "phase", "publish block", "reason", event.reason)
	}
	return pending, nil
}

type suspendedPublication struct {
	seriesID int64
	reason   string
}

const (
	savepointQuery         = `savepoint maintenance_series`
	rollbackSavepointQuery = `rollback to maintenance_series`
	releaseSavepointQuery  = `release maintenance_series`
)

// publishIsolated publishes one series inside a savepoint, so that a conflict
// or a corrupt series undoes only its own changes and the batch goes on.
func (s *Store) publishIsolated(
	ctx context.Context, tx *sql.Tx, item stagedPublication, cutoff int64,
) (Maintenance, string, error) {
	if _, err := tx.ExecContext(ctx, savepointQuery); err != nil {
		return Maintenance{}, "", fmt.Errorf("start series publication: %w", err)
	}
	publishErr := s.publishTx(ctx, tx, item.candidate, item.group, cutoff)
	if publishErr == nil {
		if _, err := tx.ExecContext(ctx, releaseSavepointQuery); err != nil {
			return Maintenance{}, "", fmt.Errorf("finish series publication: %w", err)
		}
		return Maintenance{SealedBlocks: len(item.group.blocks)}, "", nil
	}

	_, rollbackErr := tx.ExecContext(ctx, rollbackSavepointQuery)
	_, releaseErr := tx.ExecContext(ctx, releaseSavepointQuery)
	if rollbackErr != nil || releaseErr != nil {
		return Maintenance{}, "", errors.Join(publishErr, rollbackErr, releaseErr)
	}
	switch {
	case errors.Is(publishErr, ErrConflict):
		return Maintenance{Conflicts: 1}, "", nil
	case errors.Is(publishErr, ErrCorrupt):
		reason := maintenanceFailureReason("publish block", publishErr)
		suspended, err := s.suspendInPublication(ctx, tx, item.candidate.seriesID, reason)
		return Maintenance{QuarantinedSeries: suspended}, reason, err
	default:
		return Maintenance{}, "", publishErr
	}
}

func (s *Store) suspendInPublication(ctx context.Context, tx *sql.Tx, id int64, reason string) (int, error) {
	result, err := tx.ExecContext(ctx, suspendQuery, s.now().UnixMilli(), reason, id)
	if err != nil {
		return 0, fmt.Errorf("suspend failed publication: %w", err)
	}
	changed, err := result.RowsAffected()
	if err != nil {
		return 0, fmt.Errorf("count suspended publication: %w", err)
	}
	return int(changed), nil
}
