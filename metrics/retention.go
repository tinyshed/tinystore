package metrics

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"math"
)

const oldestGroupQuery = `
	select start_ts,end_ts,directory,clock_id from groups
	where series_id=? order by start_ts limit 1`

func (s *Store) firstGroup(ctx context.Context, tx *sql.Tx, id int64) (blockGroup, bool, error) {
	var start, end int64
	var clockID int64
	var data []byte
	err := tx.QueryRowContext(ctx, oldestGroupQuery, id).Scan(&start, &end, &data, &clockID)
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

const (
	headStartQuery  = `select head_start from series_state where series_id=?`
	nextExpiryQuery = `update series_state set next_gc_ts=? where series_id=?`
)

// refreshDue indexes a series by when its next expiry is due: its oldest head
// sample, or the end of its oldest live block when that comes first.
func (s *Store) refreshDue(ctx context.Context, tx *sql.Tx, id int64) error {
	var next sql.NullInt64
	err := tx.QueryRowContext(ctx, headStartQuery, id).Scan(&next)
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
	if _, err = tx.ExecContext(ctx, nextExpiryQuery, next, id); err != nil {
		return fmt.Errorf("schedule next expiry: %w", err)
	}
	return nil
}

// expireSeries removes one series' samples behind the cutoff: head points one
// by one, blocks only whole, and the series itself once nothing is left.
func (s *Store) expireSeries(ctx context.Context, id, cutoff int64) (int, bool, error) {
	expired, reclaimed := 0, false
	err := s.file.Update(ctx, func(tx *sql.Tx) error {
		maxSeen, err := readExpiryState(ctx, tx, id)
		if err != nil {
			return err
		}

		points, err := s.mutablePoints(ctx, tx, id)
		if err != nil {
			return err
		}
		kept := points[expiredPrefix(points, cutoff):]
		if len(kept) < len(points) {
			if err = s.saveHead(ctx, tx, id, kept); err != nil {
				return err
			}
		}

		expiredBlocks, err := s.expireBlocks(ctx, tx, id, cutoff)
		if err != nil {
			return err
		}
		expired = len(points) - len(kept) + expiredBlocks

		if len(kept) == 0 {
			if reclaimed, err = reclaimIfEmpty(ctx, tx, id); err != nil || reclaimed {
				return err
			}
		}

		if expired > 0 {
			if err = advanceExpiry(ctx, tx, id, s.headReady(kept, maxSeen, cutoff)); err != nil {
				return err
			}
		}
		return s.refreshDue(ctx, tx, id)
	})
	if err != nil {
		return 0, false, err
	}
	return expired, reclaimed, nil
}

const expiryStateQuery = `select version,max_seen_ts from series_state where series_id=?`

// readExpiryState returns the newest timestamp the series has shown, and
// refuses a series whose version cannot advance once more.
func readExpiryState(ctx context.Context, tx *sql.Tx, id int64) (int64, error) {
	var version, maxSeen int64
	if err := tx.QueryRowContext(ctx, expiryStateQuery, id).Scan(&version, &maxSeen); err != nil {
		return 0, fmt.Errorf("read expiry version: %w", err)
	}
	if version == math.MaxInt64 {
		return 0, fmt.Errorf("%w: series version exhausted", ErrLimit)
	}
	return maxSeen, nil
}

// expiredPrefix counts the head points before the cutoff, which lead a sorted head.
func expiredPrefix(points []Sample, cutoff int64) int {
	expired := 0
	for expired < len(points) && points[expired].At < cutoff {
		expired++
	}
	return expired
}

// expireBlocks retires the oldest group's live blocks that end before the
// cutoff; a block that ends at or after it keeps every sample it holds.
func (s *Store) expireBlocks(ctx context.Context, tx *sql.Tx, id, cutoff int64) (int, error) {
	group, found, err := s.firstGroup(ctx, tx, id)
	if err != nil || !found {
		return 0, err
	}
	expired, changed := 0, false
	for slot, block := range group.blocks {
		if !group.isLive(slot) {
			continue
		}
		if block.head.End >= cutoff {
			break
		}
		if group.isExternal(slot) {
			if err = deletePayload(ctx, tx, group.payloadID(slot)); err != nil {
				return 0, err
			}
		}
		group.live &^= uint32(1) << uint(slot) //nolint:gosec // slot belongs to the checked directory
		expired += block.head.Count
		changed = true
	}
	if !changed {
		return 0, nil
	}
	return expired, s.retireSlots(ctx, tx, id, group)
}

const deletePayloadQuery = `delete from payloads where id=?`

func deletePayload(ctx context.Context, tx *sql.Tx, payloadID int64) error {
	result, err := tx.ExecContext(ctx, deletePayloadQuery, payloadID)
	if err != nil {
		return fmt.Errorf("expire block payload: %w", err)
	}
	removed, err := result.RowsAffected()
	if err != nil {
		return fmt.Errorf("count expired payload: %w", err)
	}
	if removed != 1 {
		return fmt.Errorf("%w: expired payload missing", ErrCorrupt)
	}
	return nil
}

const (
	deleteGroupQuery  = `delete from groups where series_id=? and start_ts=?`
	rewriteGroupQuery = `update groups set directory=? where series_id=? and start_ts=?`
)

// retireSlots rewrites a group that lost blocks, keeping every payload address,
// or deletes it and releases its clock once no block is live.
func (s *Store) retireSlots(ctx context.Context, tx *sql.Tx, id int64, group blockGroup) error {
	if group.live == 0 {
		if _, err := tx.ExecContext(ctx, deleteGroupQuery, id, group.start); err != nil {
			return fmt.Errorf("remove empty group: %w", err)
		}
		return releaseClock(ctx, tx, group.clockID)
	}
	directory, err := s.writeDirectory(group)
	if err != nil {
		return err
	}
	if _, err = tx.ExecContext(ctx, rewriteGroupQuery, directory, id, group.start); err != nil {
		return fmt.Errorf("retire group slots: %w", err)
	}
	return nil
}

const hasGroupsQuery = `select exists(select 1 from groups where series_id=?)`

// reclaimIfEmpty unregisters a series whose head is empty and which has no
// group left.
func reclaimIfEmpty(ctx context.Context, tx *sql.Tx, id int64) (bool, error) {
	var hasGroup bool
	if err := tx.QueryRowContext(ctx, hasGroupsQuery, id).Scan(&hasGroup); err != nil {
		return false, fmt.Errorf("check empty series groups: %w", err)
	}
	if hasGroup {
		return false, nil
	}
	if err := reclaimSeries(ctx, tx, id); err != nil {
		return false, err
	}
	return true, nil
}

const advanceExpiryQuery = `update series_state set version=version+1,ready=? where series_id=?`

// advanceExpiry moves the version, so that a seal encoded before this expiry
// cannot publish over it.
func advanceExpiry(ctx context.Context, tx *sql.Tx, id int64, ready bool) error {
	flag := 0
	if ready {
		flag = 1
	}
	if _, err := tx.ExecContext(ctx, advanceExpiryQuery, flag, id); err != nil {
		return fmt.Errorf("advance expiry state: %w", err)
	}
	return nil
}

// reclaimSeries removes an empty series' registration and postings, the
// dictionary pairs no other series uses, and its cardinality slot.
func reclaimSeries(ctx context.Context, tx *sql.Tx, id int64) error {
	labelIDs, err := postingLabels(ctx, tx, id)
	if err != nil {
		return err
	}

	if err = decrementPostingCounts(ctx, tx, id, len(labelIDs)); err != nil {
		return err
	}

	if err = deleteRegistration(ctx, tx, id); err != nil {
		return err
	}

	if err = deleteUnusedLabels(ctx, tx, labelIDs); err != nil {
		return err
	}

	if _, err = tx.ExecContext(ctx, decreaseCardinalityQuery); err != nil {
		return fmt.Errorf("decrease cardinality: %w", err)
	}
	return nil
}

const seriesPostingsQuery = `
	select p.label_id,v.posting_count
	from postings p join label_values v on v.id=p.label_id
	where p.series_id=? order by p.label_id`

// postingLabels reads the dictionary ids of a series' postings, and closes its
// rows before the caller changes the same tables.
func postingLabels(ctx context.Context, tx *sql.Tx, id int64) ([]int64, error) {
	rows, err := tx.QueryContext(ctx, seriesPostingsQuery, id)
	if err != nil {
		return nil, fmt.Errorf("read series postings: %w", err)
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
		return nil, fmt.Errorf("read series postings: %w", errors.Join(err, closeErr))
	}
	if closeErr != nil {
		return nil, fmt.Errorf("close series postings: %w", closeErr)
	}
	if len(labelIDs) == 0 {
		return nil, fmt.Errorf("%w: empty series postings", ErrCorrupt)
	}
	return labelIDs, nil
}

const decrementPostingsQuery = `
	update label_values set posting_count=posting_count-1
	where id in (select label_id from postings where series_id=?)`

// decrementPostingCounts expects exactly one counter for each of the series' postings.
func decrementPostingCounts(ctx context.Context, tx *sql.Tx, id int64, postings int) error {
	result, err := tx.ExecContext(ctx, decrementPostingsQuery, id)
	if err != nil {
		return fmt.Errorf("decrement posting counts: %w", err)
	}
	changed, err := result.RowsAffected()
	if err != nil || changed != int64(postings) {
		return errors.Join(fmt.Errorf("%w: posting count update", ErrCorrupt), err)
	}
	return nil
}

const (
	deletePostingsQuery    = `delete from postings where series_id=?`
	deleteSeriesStateQuery = `delete from series_state where series_id=?`
	deleteSeriesQuery      = `delete from series where id=?`
)

func deleteRegistration(ctx context.Context, tx *sql.Tx, id int64) error {
	if _, err := tx.ExecContext(ctx, deletePostingsQuery, id); err != nil {
		return fmt.Errorf("remove series postings: %w", err)
	}
	if _, err := tx.ExecContext(ctx, deleteSeriesStateQuery, id); err != nil {
		return fmt.Errorf("remove series state: %w", err)
	}
	if _, err := tx.ExecContext(ctx, deleteSeriesQuery, id); err != nil {
		return fmt.Errorf("remove expired series: %w", err)
	}
	return nil
}

const (
	deleteUnusedLabelsQuery = `
		delete from label_values
		where posting_count=0 and id in (select cast(value as integer) from json_each(?))`
	decreaseCardinalityQuery = `update store_state set series_count=series_count-1 where id=1`
)

// deleteUnusedLabels removes the pairs the reclaimed series was the last to use.
func deleteUnusedLabels(ctx context.Context, tx *sql.Tx, labelIDs []int64) error {
	encoded, err := json.Marshal(labelIDs)
	if err != nil {
		return fmt.Errorf("encode expired labels: %w", err)
	}
	if _, err = tx.ExecContext(ctx, deleteUnusedLabelsQuery, string(encoded)); err != nil {
		return fmt.Errorf("remove unused labels: %w", err)
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
