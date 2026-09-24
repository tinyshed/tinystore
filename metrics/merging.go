package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"math/bits"
)

func (s *Store) mergePrecedingGroups(ctx context.Context, tx *sql.Tx, group blockGroup) (blockGroup, []blockGroup, error) {
	var replaced []blockGroup
	before := group.start
	for len(group.blocks) < groupSlots {
		var start, end, clockID int64
		var directory []byte
		err := tx.QueryRowContext(ctx, `select start_ts,end_ts,clock_id,directory from groups where series_id=? and start_ts<? order by start_ts desc limit 1`, group.seriesID, before).Scan(&start, &end, &clockID, &directory)
		if errors.Is(err, sql.ErrNoRows) {
			break
		}
		if err != nil {
			return group, nil, fmt.Errorf("find preceding group: %w", err)
		}
		clock, err := loadClock(ctx, tx, clockID, nil)
		if err != nil {
			return group, nil, err
		}
		previous, err := s.readDirectory(group.seriesID, start, end, clockID, directory, clock)
		if err != nil {
			return group, nil, err
		}
		count := bits.OnesCount32(previous.live)
		// each absorbed group at least doubles in size, bounding repeated directory rewrites
		if count > len(group.blocks) || count+len(group.blocks) > groupSlots {
			break
		}
		if previous.end >= group.start {
			return group, nil, fmt.Errorf("%w: overlapping groups during merge", ErrCorrupt)
		}
		blocks := make([]storedBlock, 0, count+len(group.blocks))
		for slot, block := range previous.blocks {
			if !previous.isLive(slot) {
				continue
			}
			if previous.isExternal(slot) {
				block.payload = previous.payloadID(slot)
			}
			blocks = append(blocks, block)
		}
		merged := group
		merged.format = 3
		merged.firstPayload = 0
		merged.blocks = append(blocks, group.blocks...)
		merged.start = merged.blocks[0].head.Start
		merged.clockBody = encodeClockGroup(merged)
		if len(merged.clockBody) > maxClockBytes {
			break
		}
		merged.live = slotsMask(len(merged.blocks))
		merged.allocation = 0
		for slot, block := range merged.blocks {
			if block.bodyBytes > inlineBytes {
				merged.allocation |= uint32(1) << uint(slot) //nolint:gosec // at most 32 slots
			}
		}
		replaced = append(replaced, previous)
		before = previous.start
		group = merged
	}
	return group, replaced, nil
}

func removeMergedGroups(ctx context.Context, tx *sql.Tx, groups []blockGroup) error {
	for _, group := range groups {
		if _, err := tx.ExecContext(ctx, `delete from groups where series_id=? and start_ts=?`, group.seriesID, group.start); err != nil {
			return fmt.Errorf("retire merged directory: %w", err)
		}
		if err := releaseClock(ctx, tx, group.clockID); err != nil {
			return err
		}
	}
	return nil
}
