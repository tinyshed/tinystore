package metrics

import (
	"context"
	"database/sql"
	"fmt"
)

type packingCandidate struct {
	modelScale        int
	seriesID, version int64
	maxSeen           int64
	cutoff            int64 // the oldest time the series keeps
	kind              Kind
	points            []Sample
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

const packingStateQuery = `
	select s.kind, state.version, state.max_seen_ts, state.model_scale, state.keep
	from series s join series_state state on s.id = state.series_id
	where s.id = ?`

func (s *Store) readCandidate(ctx context.Context, id, now int64) (packingCandidate, error) {
	ctx, cancel := context.WithTimeout(ctx, s.opts.SnapshotTimeout)
	defer cancel()
	candidate := packingCandidate{seriesID: id}
	var head headSnapshot
	var watermark int64
	err := s.file.View(ctx, func(tx *sql.Tx) error {
		var keep sql.NullInt64
		err := tx.QueryRowContext(ctx, packingStateQuery, id).
			Scan(&candidate.kind, &candidate.version, &candidate.maxSeen, &candidate.modelScale, &keep)
		if err != nil {
			return fmt.Errorf("read packing state: %w", err)
		}
		candidate.cutoff = s.retention.cutoffOf(now, keep)
		if candidate.modelScale < -2 || candidate.modelScale > 15 {
			return fmt.Errorf("%w: stored model hint", ErrCorrupt)
		}
		watermark = earlier(candidate.maxSeen, s.opts.Lateness.Milliseconds())
		var readErr error
		head, readErr = s.fetchHead(ctx, tx, id, candidate.cutoff, watermark, nil)
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
		if point.At < candidate.cutoff {
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

const clearReadyQuery = `update series_state set ready=0 where series_id=? and version=?`

func (s *Store) clearReady(ctx context.Context, candidate packingCandidate) error {
	return s.file.Update(ctx, func(tx *sql.Tx) error {
		if _, err := tx.ExecContext(ctx, clearReadyQuery, candidate.seriesID, candidate.version); err != nil {
			return fmt.Errorf("defer packing until more input: %w", err)
		}
		return nil
	})
}

func (s *Store) encodeCandidate(ctx context.Context, candidate packingCandidate) (blockGroup, error) {
	group := blockGroup{
		modelScale: candidate.modelScale, seriesID: candidate.seriesID,
		start: candidate.points[0].At, end: candidate.points[len(candidate.points)-1].At,
	}
	maxSpan := uint64(s.opts.MaxBlockSpan.Milliseconds()) //nolint:gosec // a validated positive duration
	clockBytes, directoryBytes := 6, 0
	for start := 0; start < len(candidate.points) && len(group.blocks) < groupSlots; {
		if err := ctx.Err(); err != nil {
			return group, err
		}
		end := min(start+blockSamples, len(candidate.points))
		for end > start+1 && distance(candidate.points[start].At, candidate.points[end-1].At) > maxSpan {
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
		block := storedBlock{clock: clock, summary: exactSummarize(points, candidate.kind)}
		block.head.Start = points[0].At
		block.head.End = points[len(points)-1].At
		block.head.Count = len(points)
		block.head.First = points[0].Value
		block.body = sealValueBody(block, body)
		block.bodyBytes = len(block.body)
		// Payload IDs are assigned during publication; allow their longest varint.
		descriptor := group
		if block.bodyBytes > inlineBytes {
			descriptor.allocation |= uint32(1) << len(group.blocks)
		}
		entryBytes := len(descriptor.appendBlock(nil, len(group.blocks), block)) + 9
		if len(group.blocks) > 0 && entryBytes > maxExpandedDirectory-directoryBytes {
			break
		}
		directoryBytes += entryBytes
		if block.bodyBytes > inlineBytes {
			group.allocation |= uint32(1) << len(group.blocks)
		}
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
