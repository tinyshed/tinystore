package metrics

import (
	"context"
	"database/sql"
	"encoding/json"
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

func (s *Store) expireSeries(ctx context.Context, id, cutoff int64) (int, bool, error) {
	expired := 0
	reclaimed := false
	err := s.file.Update(ctx, func(tx *sql.Tx) error {
		var version, maxSeen int64
		if err := tx.QueryRowContext(ctx, `select version,max_seen_ts from series_state where series_id=?`, id).Scan(&version, &maxSeen); err != nil {
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
		if headExpired == len(points) {
			var hasGroup bool
			if groupErr := tx.QueryRowContext(ctx, `select exists(select 1 from groups where series_id=?)`, id).Scan(&hasGroup); groupErr != nil {
				return fmt.Errorf("check empty series groups: %w", groupErr)
			}
			if !hasGroup {
				if reclaimErr := reclaimSeries(ctx, tx, id); reclaimErr != nil {
					return reclaimErr
				}
				reclaimed = true
				return nil
			}
		}
		if expired > 0 {
			ready := 0
			if s.headReady(points[headExpired:], maxSeen, cutoff) {
				ready = 1
			}
			if _, err = tx.ExecContext(ctx, `update series_state set version=version+1,ready=? where series_id=?`, ready, id); err != nil {
				return fmt.Errorf("advance expiry state: %w", err)
			}
		}
		return s.refreshDue(ctx, tx, id)
	})
	if err != nil {
		return 0, false, err
	}
	return expired, reclaimed, nil
}

func reclaimSeries(ctx context.Context, tx *sql.Tx, id int64) error {
	rows, err := tx.QueryContext(ctx, `select p.label_id,v.posting_count from postings p join label_values v on v.id=p.label_id where p.series_id=? order by p.label_id`, id)
	if err != nil {
		return fmt.Errorf("read series postings: %w", err)
	}
	var labelIDs []int64
	for rows.Next() {
		var labelID, count int64
		if err = rows.Scan(&labelID, &count); err != nil {
			break
		}
		if count < 1 {
			err = fmt.Errorf("%w: posting count before reclamation", ErrCorrupt)
			break
		}
		labelIDs = append(labelIDs, labelID)
	}
	if err == nil {
		err = rows.Err()
	}
	closeErr := rows.Close() //nolint:sqlclosecheck // release rows before updating the same tables
	if err != nil {
		return fmt.Errorf("read series postings: %w", errors.Join(err, closeErr))
	}
	if closeErr != nil {
		return fmt.Errorf("close series postings: %w", closeErr)
	}
	if len(labelIDs) == 0 {
		return fmt.Errorf("%w: empty series postings", ErrCorrupt)
	}
	result, err := tx.ExecContext(ctx, `update label_values set posting_count=posting_count-1 where id in (select label_id from postings where series_id=?)`, id)
	if err != nil {
		return fmt.Errorf("decrement posting counts: %w", err)
	}
	changed, err := result.RowsAffected()
	if err != nil || changed != int64(len(labelIDs)) { //nolint:gosec // each registered series has at most 128 labels
		return errors.Join(fmt.Errorf("%w: posting count update", ErrCorrupt), err)
	}
	if _, err = tx.ExecContext(ctx, `delete from postings where series_id=?`, id); err != nil {
		return fmt.Errorf("remove series postings: %w", err)
	}
	if _, err = tx.ExecContext(ctx, `delete from series_state where series_id=?`, id); err != nil {
		return fmt.Errorf("remove series state: %w", err)
	}
	if _, err = tx.ExecContext(ctx, `delete from series where id=?`, id); err != nil {
		return fmt.Errorf("remove expired series: %w", err)
	}
	encoded, err := json.Marshal(labelIDs)
	if err != nil {
		return fmt.Errorf("encode expired labels: %w", err)
	}
	if _, err = tx.ExecContext(ctx, `delete from label_values where posting_count=0 and id in (select cast(value as integer) from json_each(?))`, string(encoded)); err != nil {
		return fmt.Errorf("remove unused labels: %w", err)
	}
	if _, err = tx.ExecContext(ctx, `update store_state set series_count=series_count-1 where id=1`); err != nil {
		return fmt.Errorf("decrease cardinality: %w", err)
	}
	return nil
}

func earlier(at, delta int64) int64 {
	if at < math.MinInt64+delta {
		return math.MinInt64
	}
	return at - delta
}

func (s *Store) cutoff() int64 { return earlier(s.now().UnixMilli(), s.opts.Retention.Milliseconds()) }
