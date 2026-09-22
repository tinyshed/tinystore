package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"math"
	"math/bits"
)

type packingCandidate struct {
	modelScale        int
	seriesID, version int64
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
	cutoff := s.cutoff()
	due, err := s.dueSeries(ctx, false, cutoff)
	if err != nil {
		return result, err
	}
	for _, id := range due {
		expired, expireErr := s.expireSeries(ctx, id, cutoff)
		if expireErr != nil {
			return result, expireErr
		}
		result.ExpiredSamples += expired
		s.expired.Add(uint64(expired)) //nolint:gosec // expiry only counts removed samples
	}
	ready, err := s.dueSeries(ctx, true, cutoff)
	if err != nil {
		return result, err
	}
	for _, id := range ready {
		candidate, readErr := s.readCandidate(ctx, id, cutoff)
		if readErr != nil {
			return result, readErr
		}
		if len(candidate.points) < blockSamples {
			if err = s.clearReady(ctx, candidate); err != nil {
				return result, err
			}
			continue
		}
		group, encodeErr := s.encodeCandidate(ctx, candidate)
		if encodeErr != nil {
			return result, encodeErr
		}
		consumed := 0
		for _, block := range group.blocks {
			consumed += block.head.Count
		}
		candidate.points = candidate.points[:consumed]
		if err = s.publish(ctx, candidate, group); errors.Is(err, ErrConflict) {
			result.Conflicts++
			continue
		} else if err != nil {
			return result, err
		}
		result.SealedBlocks += len(group.blocks)
		s.sealed.Add(uint64(len(group.blocks))) //nolint:gosec // the group has 1..32 blocks
	}
	return result, nil
}

func (s *Store) dueSeries(ctx context.Context, packing bool, cutoff int64) ([]int64, error) {
	ctx, cancel := context.WithTimeout(ctx, s.opts.SnapshotTimeout)
	defer cancel()
	var ids []int64
	err := s.file.View(ctx, func(tx *sql.Tx) error {
		query := `select series_id from series_state where next_gc_ts is not null and next_gc_ts<? order by next_gc_ts,series_id limit ?`
		arguments := []any{cutoff, s.opts.MaintenanceSeries}
		if packing {
			query = `select series_id from series_state where ready=1 order by series_id limit ?`
			arguments = []any{s.opts.MaintenanceSeries}
		}
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
	})
	return ids, err
}

func (s *Store) readCandidate(ctx context.Context, id, cutoff int64) (packingCandidate, error) {
	ctx, cancel := context.WithTimeout(ctx, s.opts.SnapshotTimeout)
	defer cancel()
	candidate := packingCandidate{seriesID: id}
	var head headSnapshot
	var watermark int64
	err := s.file.View(ctx, func(tx *sql.Tx) error {
		var maxSeen int64
		if err := tx.QueryRowContext(ctx, `select s.kind,state.version,state.max_seen_ts,state.model_scale from series s join series_state state on s.id=state.series_id where s.id=?`, id).Scan(&candidate.kind, &candidate.version, &maxSeen, &candidate.modelScale); err != nil {
			return fmt.Errorf("read packing state: %w", err)
		}
		if candidate.modelScale < -2 || candidate.modelScale > 15 {
			return fmt.Errorf("%w: stored model hint", ErrCorrupt)
		}
		watermark = earlier(maxSeen, s.opts.Lateness.Milliseconds())
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
		block := storedBlock{format: 2, clock: clock, summary: summarize(points, candidate.kind)}
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

func (s *Store) publish(ctx context.Context, candidate packingCandidate, group blockGroup) error {
	return s.file.Update(ctx, func(tx *sql.Tx) error {
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
		clockID, clockErr := acquireClock(ctx, tx, group.clockBody)
		if clockErr != nil {
			return clockErr
		}
		group.clockID = clockID
		allocated := int64(bits.OnesCount32(group.allocation))
		if allocated > 0 {
			if err := tx.QueryRowContext(ctx, `select next_payload_id from store_state where id=1`).Scan(&group.firstPayload); err != nil {
				return fmt.Errorf("read payload allocation: %w", err)
			}
			if group.firstPayload > math.MaxInt64-allocated {
				return fmt.Errorf("%w: payload identifiers exhausted", ErrLimit)
			}
			if _, err := tx.ExecContext(ctx, `update store_state set next_payload_id=? where id=1`, group.firstPayload+allocated); err != nil {
				return fmt.Errorf("reserve payload identifiers: %w", err)
			}
		}
		for slot, block := range group.blocks {
			if group.isExternal(slot) {
				if _, err := tx.ExecContext(ctx, `insert into payloads values(?,?)`, group.payloadID(slot), block.body); err != nil {
					return fmt.Errorf("write sealed payload: %w", err)
				}
			}
		}
		directory, err := s.writeDirectory(group)
		if err != nil {
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
		if _, err = tx.ExecContext(ctx, `update series_state set sealed_before=?,version=version+1,ready=case when head_count>=? then 1 else 0 end,model_scale=? where series_id=?`, group.end+1, blockSamples, group.modelScale, group.seriesID); err != nil {
			return fmt.Errorf("advance sealed frontier: %w", err)
		}
		return s.refreshDue(ctx, tx, group.seriesID)
	})
}
