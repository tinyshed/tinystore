package metrics

import (
	"context"
	"database/sql"
	"fmt"
	"math"
)

type packingCandidate struct {
	modelScale        int
	seriesID, version int64
	maxSeen           int64
	kind              Kind
	points            []Sample
}

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

func (s *Store) headReady(points []Sample, maxSeen, cutoff int64) bool {
	watermark := earlier(maxSeen, s.opts.Lateness.Milliseconds())
	eligible := 0
	for _, point := range points {
		if point.At < cutoff {
			continue
		}
		if point.At >= watermark {
			break
		}
		eligible++
		if eligible == blockSamples {
			return true
		}
	}
	return false
}

func (s *Store) readCandidate(ctx context.Context, id, cutoff int64) (packingCandidate, error) {
	ctx, cancel := context.WithTimeout(ctx, s.opts.SnapshotTimeout)
	defer cancel()
	candidate := packingCandidate{seriesID: id}
	var head headSnapshot
	var watermark int64
	err := s.file.View(ctx, func(tx *sql.Tx) error {
		if err := tx.QueryRowContext(ctx, `select s.kind,state.version,state.max_seen_ts,state.model_scale from series s join series_state state on s.id=state.series_id where s.id=?`, id).Scan(&candidate.kind, &candidate.version, &candidate.maxSeen, &candidate.modelScale); err != nil {
			return fmt.Errorf("read packing state: %w", err)
		}
		if candidate.modelScale < -2 || candidate.modelScale > 15 {
			return fmt.Errorf("%w: stored model hint", ErrCorrupt)
		}
		watermark = earlier(candidate.maxSeen, s.opts.Lateness.Milliseconds())
		var readErr error
		head, readErr = s.fetchHead(ctx, tx, id, cutoff, watermark, nil)
		return readErr
	})
	if err != nil {
		return candidate, err
	}
	points, err := s.decodeHead(ctx, head)
	if err != nil {
		return candidate, err
	}
	for _, point := range points {
		if point.At < cutoff {
			continue
		}
		if point.At >= watermark || len(candidate.points) == blockSamples*groupSlots {
			break
		}
		candidate.points = append(candidate.points, point)
	}
	candidate.points = candidate.points[:len(candidate.points)/blockSamples*blockSamples]
	return candidate, err
}

func (s *Store) clearReady(ctx context.Context, candidate packingCandidate) error {
	return s.file.Update(ctx, func(tx *sql.Tx) error {
		if _, err := tx.ExecContext(ctx, `update series_state set ready=0 where series_id=? and version=?`, candidate.seriesID, candidate.version); err != nil {
			return fmt.Errorf("defer packing until more input: %w", err)
		}
		return nil
	})
}

func (s *Store) encodeCandidate(ctx context.Context, candidate packingCandidate) (blockGroup, error) {
	group := blockGroup{format: 2, modelScale: candidate.modelScale, seriesID: candidate.seriesID, start: candidate.points[0].At, end: candidate.points[len(candidate.points)-1].At}
	clockBytes := 6
	for start := 0; start < len(candidate.points) && len(group.blocks) < groupSlots; {
		if err := ctx.Err(); err != nil {
			return group, err
		}
		end := min(start+blockSamples, len(candidate.points))
		for end > start+1 && uint64(candidate.points[end-1].At)-uint64(candidate.points[start].At) > uint64(s.opts.MaxBlockSpan.Milliseconds()) { //nolint:gosec // modular timestamp distance and a validated positive duration
			end--
		}
		points := candidate.points[start:end]
		clock := encodeClockValues(points)
		if len(group.blocks) > 0 && clockBytes+len(clock)+32 > maxClockBytes {
			break
		}
		clockBytes += len(clock) + 32
		body, hint, err := s.encodeValues(points, group.modelScale)
		if err != nil {
			return group, fmt.Errorf("encode sealed block: %w", err)
		}
		group.modelScale = hint
		block := storedBlock{clock: clock, summary: summarize(points, candidate.kind)}
		block.head.Start = points[0].At
		block.head.End = points[len(points)-1].At
		block.head.Count = len(points)
		block.head.First = points[0].Value
		block.body = sealValueBody(block, body)
		block.bodyBytes = len(block.body)
		if block.bodyBytes > inlineBytes {
			group.allocation |= uint32(1) << uint(len(group.blocks))
		} //nolint:gosec // at most 32 slots
		group.blocks = append(group.blocks, block)
		start = end
	}
	group.live = slotsMask(len(group.blocks))
	group.end = group.blocks[len(group.blocks)-1].head.End
	group.clockBody = encodeClockGroup(group)
	if len(group.clockBody) > maxClockBytes {
		return group, fmt.Errorf("%w: clock group size", ErrLimit)
	}
	return group, nil
}

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
