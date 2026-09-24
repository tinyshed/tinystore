package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"math/bits"
)

// mergePrecedingGroups lets a new group absorb the groups before it, one at a
// time, for as long as absorb accepts them. Payloads keep their addresses, so
// only directories and clocks are rewritten.
func (s *Store) mergePrecedingGroups(
	ctx context.Context, tx *sql.Tx, group blockGroup,
) (blockGroup, []blockGroup, error) {
	var replaced []blockGroup
	before := group.start
	for len(group.blocks) < groupSlots {
		previous, found, err := s.precedingGroup(ctx, tx, group.seriesID, before)
		if err != nil {
			return group, nil, err
		}
		if !found {
			break
		}
		merged, fits, err := absorb(previous, group)
		if err != nil {
			return group, nil, err
		}
		if !fits {
			break
		}
		replaced = append(replaced, previous)
		before = previous.start
		group = merged
	}
	return group, replaced, nil
}

const precedingGroupQuery = `
	select start_ts,end_ts,clock_id,directory from groups
	where series_id=? and start_ts<? order by start_ts desc limit 1`

func (s *Store) precedingGroup(ctx context.Context, tx *sql.Tx, seriesID, before int64) (blockGroup, bool, error) {
	var row groupRow
	err := tx.QueryRowContext(ctx, precedingGroupQuery, seriesID, before).
		Scan(&row.start, &row.end, &row.clockID, &row.data)
	if errors.Is(err, sql.ErrNoRows) {
		return blockGroup{}, false, nil
	}
	if err != nil {
		return blockGroup{}, false, fmt.Errorf("find preceding group: %w", err)
	}
	clock, err := loadClock(ctx, tx, row.clockID, nil)
	if err != nil {
		return blockGroup{}, false, err
	}
	previous, err := s.readDirectory(seriesID, row, clock)
	return previous, true, err
}

// absorb puts the live blocks of the preceding group in front of the group's
// own. It declines a preceding group with more live blocks than the group, so
// that each absorbed group at least doubles in size and bounds how often a
// directory is rewritten, and declines one that does not fit beside it.
func absorb(previous, group blockGroup) (blockGroup, bool, error) {
	count := bits.OnesCount32(previous.live)
	if count > len(group.blocks) || count+len(group.blocks) > groupSlots {
		return group, false, nil
	}
	if previous.end >= group.start {
		return group, false, fmt.Errorf("%w: overlapping groups during merge", ErrCorrupt)
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
		return group, false, nil
	}
	merged.live = slotsMask(len(merged.blocks))
	merged.allocation = 0
	for slot, block := range merged.blocks {
		if block.bodyBytes > inlineBytes {
			merged.allocation |= uint32(1) << slot
		}
	}
	return merged, true, nil
}

func removeMergedGroups(ctx context.Context, tx *sql.Tx, groups []blockGroup) error {
	for _, group := range groups {
		if _, err := tx.ExecContext(ctx, deleteGroupQuery, group.seriesID, group.start); err != nil {
			return fmt.Errorf("retire merged directory: %w", err)
		}
		if err := releaseClock(ctx, tx, group.clockID); err != nil {
			return err
		}
	}
	return nil
}
