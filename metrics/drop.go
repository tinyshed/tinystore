package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// DropSeries removes one series and everything it holds in one transaction,
// whether its data still reads or not, including a suspended series. A group
// whose directory or clock no longer reads is removed without the payload rows
// it named: they stay in the file, and UnreadableGroups counts such groups.
func (s *Store) DropSeries(ctx context.Context, labels []Label) (DroppedSeries, error) {
	if err := s.enter(ctx); err != nil {
		return DroppedSeries{}, err
	}
	defer s.leave()

	release, err := s.holdMaintenance(ctx)
	if err != nil {
		return DroppedSeries{}, err
	}
	defer release()

	ordered, identity, err := canonicalLabels(labels, true)
	if err != nil {
		return DroppedSeries{}, err
	}

	var dropped DroppedSeries
	var suspended bool
	err = s.file.Update(ctx, func(tx *sql.Tx) error {
		dropped, suspended, err = s.dropSeries(ctx, tx, preparedBatch{identity: identity, labels: ordered})
		return err
	})
	if err != nil {
		return DroppedSeries{}, fmt.Errorf("drop series: %w", err)
	}
	if suspended {
		s.quarantined.Add(-1)
	}
	return dropped, nil
}

const droppedSeriesQuery = `
	select s.id, s.label_ids, state.failed_at is not null
	from series s left join series_state state on state.series_id = s.id
	where s.identity = ?`

// dropSeries finds the series by its full labels, then removes its groups and
// its registration; the head goes with its state row.
func (s *Store) dropSeries(ctx context.Context, tx *sql.Tx, series preparedBatch) (DroppedSeries, bool, error) {
	var id int64
	var stored []byte
	var suspended bool
	err := tx.QueryRowContext(ctx, droppedSeriesQuery, seriesIdentity(series.identity)).Scan(&id, &stored, &suspended)
	if errors.Is(err, sql.ErrNoRows) {
		return DroppedSeries{}, false, nil
	}
	if err != nil {
		return DroppedSeries{}, false, fmt.Errorf("find series: %w", err)
	}
	if err = confirmIdentity(ctx, tx, stored, series); err != nil {
		return DroppedSeries{}, false, err
	}

	unreadable, err := s.dropGroups(ctx, tx, id)
	if err != nil {
		return DroppedSeries{}, false, err
	}

	if err = reclaimSeries(ctx, tx, id); err != nil {
		return DroppedSeries{}, false, err
	}
	return DroppedSeries{Found: true, UnreadableGroups: unreadable}, suspended, nil
}

const seriesGroupsQuery = `select start_ts,end_ts,directory,clock_id from groups where series_id=? order by start_ts`

// dropGroups deletes every group of a series with the payloads its directory
// names, and releases its clock; it returns how many groups did not read.
func (s *Store) dropGroups(ctx context.Context, tx *sql.Tx, id int64) (int, error) {
	rows, err := tx.QueryContext(ctx, seriesGroupsQuery, id) //nolint:rowserrcheck // EachRow checks Err
	if err != nil {
		return 0, fmt.Errorf("find series groups: %w", err)
	}
	var groups []groupRow
	err = sqlite.EachRow(rows, "series groups", func(rows *sql.Rows) error {
		var row groupRow
		if scanErr := rows.Scan(&row.start, &row.end, &row.data, &row.clockID); scanErr != nil {
			return scanErr
		}
		groups = append(groups, row)
		return nil
	})
	if err != nil {
		return 0, err
	}

	unreadable := 0
	for _, row := range groups {
		read, err := s.dropGroup(ctx, tx, id, row)
		if err != nil {
			return 0, err
		}
		if !read {
			unreadable++
		}
	}
	return unreadable, nil
}

// dropGroup deletes one group and reports whether its directory and clock
// read; only a directory that reads, and so proves it is this series', may
// name the payload rows to delete.
func (s *Store) dropGroup(ctx context.Context, tx *sql.Tx, id int64, row groupRow) (bool, error) {
	payloads, err := s.groupPayloads(ctx, tx, id, row)
	read := err == nil
	if !read && !errors.Is(err, ErrCorrupt) {
		return false, err
	}
	for _, payload := range payloads {
		// a payload the file already lost needs no deleting
		if _, err = tx.ExecContext(ctx, deletePayloadQuery, payload); err != nil {
			return false, fmt.Errorf("remove dropped payload: %w", err)
		}
	}

	if _, err = tx.ExecContext(ctx, deleteGroupQuery, id, row.start); err != nil {
		return false, fmt.Errorf("remove dropped group: %w", err)
	}
	if err = releaseClock(ctx, tx, row.clockID); err != nil && !errors.Is(err, ErrCorrupt) {
		return false, err
	}
	return read && err == nil, nil
}

// groupPayloads lists the payload rows a group still owns: its live blocks
// with an external body.
func (s *Store) groupPayloads(ctx context.Context, tx *sql.Tx, id int64, row groupRow) ([]int64, error) {
	clock, err := loadClock(ctx, tx, row.clockID, nil)
	if err != nil {
		return nil, err
	}
	group, err := s.readDirectory(id, row, clock)
	if err != nil {
		return nil, err
	}
	var payloads []int64
	for slot := range group.blocks {
		if group.isLive(slot) && group.isExternal(slot) {
			payloads = append(payloads, group.payloadID(slot))
		}
	}
	return payloads, nil
}
