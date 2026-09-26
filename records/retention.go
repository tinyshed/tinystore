package records

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// segments expire a batch to a transaction, so one pass over a long outage
// does not hold the writer for all of it
const expireBatch = 64

// expire removes whole segments whose newest record is past the cutoff, with
// the places merged into them, and head rows likewise. A read clips what is
// left by the same kind of cutoff, so a segment retention has partly passed
// answers only what it keeps.
func (p *maintenancePass) expire(ctx context.Context) error {
	for {
		removed, err := p.store.expireSegments(ctx, p.cutoff())
		if err != nil {
			return err
		}
		p.result.ExpiredSegments += removed
		p.store.expired.Add(unsigned(removed))
		if removed < expireBatch {
			break
		}
	}
	removed, err := p.store.expireHeads(ctx, p.cutoff())
	p.result.ExpiredHeads += removed
	return err
}

const (
	selectExpiredSegments = `
		select id, first_block, last_block from segments
		where last_at < ? and holder is null
		order by id
		limit cast(? as integer)`
	deleteBlockFilters = `delete from block_filters where block between ? and ?`
	deleteBlockTraces  = `delete from block_traces where block between ? and ?`
	deleteBlocks       = `delete from blocks where id between ? and ?`
	deleteSegmentKeys  = `delete from segment_keys where segment = ?`
	// a holder's places come after it, so they are found without an index
	deleteMergedPlaces = `delete from segments where id > ?1 and holder = ?1`
	deleteSegment      = `delete from segments where id = ?`
)

// expiredSegment is a segment and the range of its blocks' ids
type expiredSegment struct {
	id, firstBlock, lastBlock int64
}

func (s *Store) expireSegments(ctx context.Context, cutoff int64) (int, error) {
	var expired []expiredSegment
	err := s.file.UpdatePrepared(ctx, func(tx sqlite.Writer) error {
		//nolint:rowserrcheck // EachRow checks Err
		rows, err := tx.QueryContext(ctx, selectExpiredSegments, cutoff, expireBatch)
		if err != nil {
			return err
		}
		err = sqlite.EachRow(rows, "expired segments", func(rows *sql.Rows) error {
			var segment expiredSegment
			scanErr := rows.Scan(&segment.id, &segment.firstBlock, &segment.lastBlock)
			expired = append(expired, segment)
			return scanErr
		})
		if err != nil {
			return err
		}
		for _, segment := range expired {
			if err = deleteExpiredSegment(ctx, tx, segment); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		return 0, fmt.Errorf("records: expire segments: %w", err)
	}
	for _, segment := range expired {
		s.damaged.forget(Damage{Segment: segment.id})
	}
	return len(expired), nil
}

func deleteExpiredSegment(ctx context.Context, tx sqlite.Writer, segment expiredSegment) error {
	for _, statement := range []string{deleteBlockFilters, deleteBlockTraces, deleteBlocks} {
		if _, err := tx.ExecContext(ctx, statement, segment.firstBlock, segment.lastBlock); err != nil {
			return err
		}
	}
	for _, statement := range []string{deleteSegmentKeys, deleteMergedPlaces, deleteSegment} {
		if _, err := tx.ExecContext(ctx, statement, segment.id); err != nil {
			return err
		}
	}
	return nil
}

const selectExpiredHeads = `select id, stream, late, count, input from heads where last_at < ?`

// expiredHead is what leaves one head: its rows, their records and their input
type expiredHead struct {
	ids          []int64
	count, input int
}

func (s *Store) expireHeads(ctx context.Context, cutoff int64) (int, error) {
	removed, gone := 0, []int64(nil)
	err := s.file.UpdatePrepared(ctx, func(tx sqlite.Writer) error {
		heads, err := expiredHeadRows(ctx, tx, cutoff)
		if err != nil {
			return err
		}
		for head, expired := range heads {
			for _, id := range expired.ids {
				if _, err = tx.ExecContext(ctx, deleteHeadRow, id); err != nil {
					return err
				}
			}
			if _, err = settleHead(ctx, tx, head, expired.count, expired.input); err != nil {
				return err
			}
			removed += len(expired.ids)
			gone = append(gone, expired.ids...)
		}
		return nil
	})
	if err != nil {
		return 0, fmt.Errorf("records: expire head rows: %w", err)
	}
	for _, id := range gone {
		s.damaged.forget(Damage{HeadRow: id})
	}
	return removed, nil
}

func expiredHeadRows(ctx context.Context, tx sqlite.Writer, cutoff int64) (map[headKey]*expiredHead, error) {
	heads := map[headKey]*expiredHead{}
	rows, err := tx.QueryContext(ctx, selectExpiredHeads, cutoff) //nolint:rowserrcheck // EachRow checks Err
	if err != nil {
		return nil, err
	}
	err = sqlite.EachRow(rows, "expired head rows", func(rows *sql.Rows) error {
		var id int64
		var head headKey
		var count, input int
		if scanErr := rows.Scan(&id, &head.stream, &head.late, &count, &input); scanErr != nil {
			return scanErr
		}
		if heads[head] == nil {
			heads[head] = &expiredHead{}
		}
		expired := heads[head]
		expired.ids, expired.count, expired.input = append(expired.ids, id), expired.count+count, expired.input+input
		return nil
	})
	return heads, err
}
