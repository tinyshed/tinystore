package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"math"
)

func (s *Store) firstGroup(ctx context.Context, tx *sql.Tx, id int64) (blockGroup, bool, error) {
	var start, end int64
	var clockID int64
	var data []byte
	err := tx.QueryRowContext(ctx, `select start_ts,end_ts,directory,clock_id from groups where series_id=? order by start_ts limit 1`, id).Scan(&start, &end, &data, &clockID)
	if errors.Is(err, sql.ErrNoRows) {
		return blockGroup{}, false, nil
	}
	if err != nil {
		return blockGroup{}, false, fmt.Errorf("read oldest group: %w", err)
	}
	clock, err := loadClock(ctx, tx, clockID, nil)
	if err != nil {
		return blockGroup{}, false, err
	}
	group, err := s.readDirectory(id, start, end, clockID, data, clock)
	return group, true, err
}

func (s *Store) refreshDue(ctx context.Context, tx *sql.Tx, id int64) error {
	var next sql.NullInt64
	err := tx.QueryRowContext(ctx, `select head_start from series_state where series_id=?`, id).Scan(&next)
	if err != nil {
		return fmt.Errorf("find next head expiry: %w", err)
	}
	group, found, err := s.firstGroup(ctx, tx, id)
	if err != nil {
		return err
	}
	if found {
		for slot, block := range group.blocks {
			if group.isLive(slot) {
				if !next.Valid || block.head.End < next.Int64 {
					next = sql.NullInt64{Int64: block.head.End, Valid: true}
				}
				break
			}
		}
	}
	if _, err = tx.ExecContext(ctx, `update series_state set next_gc_ts=? where series_id=?`, next, id); err != nil {
		return fmt.Errorf("schedule next expiry: %w", err)
	}
	return nil
}

func (s *Store) expireSeries(ctx context.Context, id, cutoff int64) (int, error) {
	expired := 0
	err := s.file.Update(ctx, func(tx *sql.Tx) error {
		var version int64
		if err := tx.QueryRowContext(ctx, `select version from series_state where series_id=?`, id).Scan(&version); err != nil {
			return fmt.Errorf("read expiry version: %w", err)
		}
		if version == math.MaxInt64 {
			return fmt.Errorf("%w: series version exhausted", ErrLimit)
		}
		points, err := s.mutablePoints(ctx, tx, id)
		if err != nil {
			return err
		}
		headExpired := 0
		for headExpired < len(points) && points[headExpired].At < cutoff {
			headExpired++
		}
		if headExpired > 0 {
			if err = s.saveHead(ctx, tx, id, points[headExpired:]); err != nil {
				return err
			}
		}
		expired = headExpired
		group, found, err := s.firstGroup(ctx, tx, id)
		if err != nil {
			return err
		}
		changed := false
		if found {
			for slot, block := range group.blocks {
				if !group.isLive(slot) {
					continue
				}
				if block.head.End >= cutoff {
					break
				}
				if group.isExternal(slot) {
					result, deleteErr := tx.ExecContext(ctx, `delete from payloads where id=?`, group.payloadID(slot))
					if deleteErr != nil {
						return fmt.Errorf("expire block payload: %w", deleteErr)
					}
					removed, countErr := result.RowsAffected()
					if countErr != nil {
						return fmt.Errorf("count expired payload: %w", countErr)
					}
					if removed != 1 {
						return fmt.Errorf("%w: expired payload missing", ErrCorrupt)
					}
				}
				group.live &^= uint32(1) << uint(slot) //nolint:gosec // slot belongs to the checked directory
				expired += block.head.Count
				changed = true
			}
		}
		if changed {
			if group.live == 0 {
				if _, err = tx.ExecContext(ctx, `delete from groups where series_id=? and start_ts=?`, id, group.start); err != nil {
					return fmt.Errorf("remove empty group: %w", err)
				}
				if err = releaseClock(ctx, tx, group.clockID); err != nil {
					return err
				}
			} else {
				directory, encodeErr := s.writeDirectory(group)
				if encodeErr != nil {
					return encodeErr
				}
				if _, err = tx.ExecContext(ctx, `update groups set directory=? where series_id=? and start_ts=?`, directory, id, group.start); err != nil {
					return fmt.Errorf("retire group slots: %w", err)
				}
			}
		}
		if expired > 0 {
			if _, err = tx.ExecContext(ctx, `update series_state set version=version+1,ready=case when head_count<? then 0 else ready end where series_id=?`, blockSamples, id); err != nil {
				return fmt.Errorf("advance expiry state: %w", err)
			}
		}
		return s.refreshDue(ctx, tx, id)
	})
	if err != nil {
		return 0, err
	}
	return expired, nil
}
