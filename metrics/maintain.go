package metrics

import (
	"context"
	"database/sql"
	"fmt"
)

// Maintain performs a bounded retention pass, then seals eligible full microblocks.
func (s *Store) Maintain(ctx context.Context) (Maintenance, error) {
	var result Maintenance
	if err := s.enter(ctx); err != nil {
		return result, err
	}
	defer s.leave()
	select {
	case <-s.maintenanceGate:
	case <-ctx.Done():
		return result, ctx.Err()
	}
	defer func() { s.maintenanceGate <- struct{}{} }()
	if s.opts.SharedBudget != nil {
		weight, reserveErr := s.maintenanceReservation()
		if reserveErr != nil {
			return result, reserveErr
		}
		if reserveErr := s.opts.SharedBudget.acquire(ctx, weight); reserveErr != nil {
			return result, reserveErr
		}
		defer s.opts.SharedBudget.release(weight)
	}
	cutoff := s.cutoff()
	due, err := s.dueSeries(ctx, false, cutoff)
	if err != nil {
		return result, err
	}
	for _, id := range due {
		expired, reclaimed, expireErr := s.expireSeries(ctx, id, cutoff)
		if expireErr != nil {
			quarantined, failureErr := s.handleMaintenanceFailure(ctx, id, "retention", expireErr, true)
			if failureErr != nil {
				return result, failureErr
			}
			if quarantined {
				result.QuarantinedSeries++
			}
			continue
		}
		result.ExpiredSamples += expired
		s.expired.Add(uint64(expired)) //nolint:gosec // expiry only counts removed samples
		if reclaimed {
			result.ReclaimedSeries++
			s.reclaimed.Add(1)
		}
	}
	ready, err := s.dueSeries(ctx, true, cutoff)
	if err != nil {
		return result, err
	}
	var staged []stagedPublication
	stagedBytes := 0
	flush := func() error {
		committed, publishErr := s.publishBatch(ctx, staged, cutoff)
		if publishErr != nil {
			return publishErr
		}
		result.SealedBlocks += committed.SealedBlocks
		result.Conflicts += committed.Conflicts
		result.QuarantinedSeries += committed.QuarantinedSeries
		s.sealed.Add(uint64(committed.SealedBlocks))          //nolint:gosec // only committed blocks are counted
		s.quarantined.Add(int64(committed.QuarantinedSeries)) //nolint:gosec // bounded by staged series count
		staged = nil
		stagedBytes = 0
		return nil
	}
	for _, id := range ready {
		candidate, readErr := s.readCandidate(ctx, id, cutoff)
		if readErr != nil {
			quarantined, failureErr := s.handleMaintenanceFailure(ctx, id, "read head", readErr, true)
			if failureErr != nil {
				return result, failureErr
			}
			if quarantined {
				result.QuarantinedSeries++
			}
			continue
		}
		if len(candidate.points) < blockSamples {
			if err = s.clearReady(ctx, candidate); err != nil {
				return result, err
			}
			continue
		}
		group, encodeErr := s.encodeCandidate(ctx, candidate)
		if encodeErr != nil {
			quarantined, failureErr := s.handleMaintenanceFailure(ctx, id, "encode block", encodeErr, true)
			if failureErr != nil {
				return result, failureErr
			}
			if quarantined {
				result.QuarantinedSeries++
			}
			continue
		}
		consumed := 0
		for _, block := range group.blocks {
			consumed += block.head.Count
		}
		candidate.points = candidate.points[:consumed]
		bytes := publicationBytes(candidate, group)
		if len(staged) > 0 && (len(staged) == publicationBatchSeries || stagedBytes+bytes > publicationBatchBytes) {
			if err := flush(); err != nil {
				return result, err
			}
		}
		staged = append(staged, stagedPublication{candidate: candidate, group: group})
		stagedBytes += bytes
		if len(staged) == publicationBatchSeries || stagedBytes >= publicationBatchBytes {
			if err := flush(); err != nil {
				return result, err
			}
		}
	}
	if err := flush(); err != nil {
		return result, err
	}
	return result, nil
}

func (s *Store) dueSeries(ctx context.Context, packing bool, cutoff int64) ([]int64, error) {
	ctx, cancel := context.WithTimeout(ctx, s.opts.SnapshotTimeout)
	defer cancel()
	var ids []int64
	err := s.file.View(ctx, func(tx *sql.Tx) error {
		readIDs := func(query string, arguments ...any) error {
			rows, err := tx.QueryContext(ctx, query, arguments...)
			if err != nil {
				return fmt.Errorf("find due series: %w", err)
			}
			defer rows.Close()
			for rows.Next() {
				var id int64
				if err = rows.Scan(&id); err != nil {
					return fmt.Errorf("read due series: %w", err)
				}
				ids = append(ids, id)
			}
			if err = rows.Err(); err != nil {
				return fmt.Errorf("iterate due series: %w", err)
			}
			return nil
		}
		if !packing {
			return readIDs(`select series_id from series_state where failed_at is null and next_gc_ts is not null and next_gc_ts<? order by next_gc_ts,series_id limit cast(? as integer)`, cutoff, s.opts.MaintenanceSeries)
		}
		cursor := s.readyCursor.Load()
		if err := readIDs(`select series_id from series_state where failed_at is null and ready=1 and series_id>? order by series_id limit cast(? as integer)`, cursor, s.opts.MaintenanceSeries); err != nil {
			return err
		}
		if len(ids) < s.opts.MaintenanceSeries && cursor > 0 {
			return readIDs(`select series_id from series_state where failed_at is null and ready=1 and series_id<=? order by series_id limit cast(? as integer)`, cursor, s.opts.MaintenanceSeries-len(ids))
		}
		return nil
	})
	if err == nil && packing && len(ids) > 0 {
		s.readyCursor.Store(ids[len(ids)-1])
	}
	return ids, err
}
