package records

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"slices"
	"sync"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// damaged is the rows this handle met that no longer read, each logged once;
// the file is the record of them, so a reopened store finds them again as it
// meets them
type damaged struct {
	mu    sync.Mutex
	found map[damageKey]Damage
	order []damageKey
}

type damageKey struct {
	segment, headRow int64
}

func keyOf(damage Damage) damageKey {
	return damageKey{segment: damage.Segment, headRow: damage.HeadRow}
}

// note remembers a damaged row, and says whether it is new to this handle
func (d *damaged) note(damage Damage) bool {
	d.mu.Lock()
	defer d.mu.Unlock()
	key := keyOf(damage)
	if _, known := d.found[key]; known {
		return false
	}
	d.found[key] = damage
	d.order = append(d.order, key)
	return true
}

func (d *damaged) headRow(id int64) bool {
	d.mu.Lock()
	defer d.mu.Unlock()
	_, known := d.found[damageKey{headRow: id}]
	return known
}

func (d *damaged) forget(damage Damage) {
	d.mu.Lock()
	defer d.mu.Unlock()
	key := keyOf(damage)
	delete(d.found, key)
	d.order = slices.DeleteFunc(d.order, func(known damageKey) bool { return known == key })
}

func (d *damaged) list() []Damage {
	d.mu.Lock()
	defer d.mu.Unlock()
	list := make([]Damage, len(d.order))
	for i, key := range d.order {
		list[i] = d.found[key]
	}
	return list
}

// damageOf is err as the damage of one row, when it is a corruption of that
// row's bytes; any other error is itself
func damageOf(found Damage, err error) error {
	if !errors.Is(err, tinystore.ErrCorrupt) {
		return err
	}
	found.Reason = err.Error()
	return &DamageError{Damage: found, Err: err}
}

// noted remembers the damage err carries and logs it the first time
func (s *Store) noted(err error) error {
	var damage *DamageError
	if errors.As(err, &damage) && s.damaged.note(damage.Damage) {
		found := damage.Damage
		s.log.Error("a row no longer reads", "stream", found.Stream, "segment", found.Segment,
			"head_row", found.HeadRow, "from", found.From, "to", found.To, "reason", found.Reason)
	}
	return err
}

// Damaged lists the rows this handle has met that no longer read, in the order
// it met them: what Drop removes.
func (s *Store) Damaged() []Damage {
	return s.damaged.list()
}

// Drop removes a damaged head row, or a damaged segment with all its blocks,
// in one transaction, and forgets it. It refuses a row that still reads with
// tinystore.ErrConflict, so that it removes only what is lost already. Follow
// counts a dropped segment in Batch.Expired, as it counts retention's.
func (s *Store) Drop(ctx context.Context, damage Damage) error {
	release, err := s.admit(ctx)
	if err != nil {
		return err
	}
	defer release()

	hold, err := s.holdMaintenance(ctx)
	if err != nil {
		return err
	}
	defer hold()

	unreserve, err := s.reserve(ctx, func() int64 { return segmentReservation })
	if err != nil {
		return err
	}
	defer unreserve()

	if err = s.dropDamaged(ctx, damage); err != nil {
		return fmt.Errorf("records: drop: %w", err)
	}
	s.damaged.forget(damage)
	return nil
}

func (s *Store) dropDamaged(ctx context.Context, damage Damage) error {
	switch {
	case damage.Segment > 0 && damage.HeadRow == 0:
		return s.dropSegment(ctx, damage.Segment)
	case damage.HeadRow > 0 && damage.Segment == 0:
		return s.dropHeadRow(ctx, damage.HeadRow)
	}
	return fmt.Errorf("%w: a damage names one segment or one head row, not %+v", tinystore.ErrInvalid, damage)
}

const selectHeadRowToDrop = `select stream, late, count, input, body from heads where id = ?`

// dropHeadRow removes a head row that no longer reads and settles its head
func (s *Store) dropHeadRow(ctx context.Context, id int64) error {
	var head headKey
	emptied := false
	err := s.file.UpdatePrepared(ctx, func(tx sqlite.Writer) error {
		var count, input int
		var body []byte
		err := sqlite.QueryRow(ctx, tx, selectHeadRowToDrop, id).Scan(&head.stream, &head.late, &count, &input, &body)
		if errors.Is(err, sql.ErrNoRows) {
			return nil
		}
		if err != nil {
			return err
		}
		if _, err = newDecoder(s.unpack).parseHeadRow(s.streams.name(head.stream), body); err == nil {
			return fmt.Errorf("%w: head row %d reads", tinystore.ErrConflict, id)
		}
		if _, err = tx.ExecContext(ctx, deleteHeadRow, id); err != nil {
			return err
		}
		emptied, err = settleHead(ctx, tx, head, count, input)
		return err
	})
	if err == nil && emptied && !head.late {
		s.waiting.forget(s.streams.name(head.stream))
	}
	return err
}

// a segment is dropped by its id, so that a block whose segment row is gone
// goes too
const (
	selectSegmentToDrop  = `select body from segments where id = ?`
	selectBlocksToDrop   = `select body from blocks where segment = ? order by id`
	deleteSegmentFilters = `delete from block_filters where block in (select id from blocks where segment = ?)`
	deleteSegmentTraces  = `delete from block_traces where block in (select id from blocks where segment = ?)`
	deleteSegmentBlocks  = `delete from blocks where segment = ?`
)

// dropSegment removes a segment that no longer reads, whole: a block cannot go
// alone, since a Follow cursor counts a segment's rows through its blocks
func (s *Store) dropSegment(ctx context.Context, id int64) error {
	return s.file.UpdatePrepared(ctx, func(tx sqlite.Writer) error {
		found, reads, err := s.segmentReads(ctx, tx, id)
		if err != nil || !found {
			return err
		}
		if reads {
			return fmt.Errorf("%w: segment %d reads", tinystore.ErrConflict, id)
		}
		for _, statement := range []string{
			deleteSegmentFilters, deleteSegmentTraces, deleteSegmentBlocks, deleteSegmentKeys, deleteSegment,
		} {
			if _, err = tx.ExecContext(ctx, statement, id); err != nil {
				return err
			}
		}
		return nil
	})
}

// segmentReads decodes a segment's row and every one of its blocks whole
func (s *Store) segmentReads(ctx context.Context, tx sqlite.Writer, id int64) (found, reads bool, err error) {
	var row []byte
	err = sqlite.QueryRow(ctx, tx, selectSegmentToDrop, id).Scan(&row)
	missing := errors.Is(err, sql.ErrNoRows)
	if err != nil && !missing {
		return false, false, err
	}
	blocks, err := segmentBlocks(ctx, tx, id)
	if err != nil || missing {
		return len(blocks) > 0, false, err
	}
	d := newDecoder(s.unpack)
	schema, err := d.parseSchema(row)
	if err != nil {
		return true, false, nil //nolint:nilerr // a segment row that does not read is what Drop removes
	}
	for _, body := range blocks {
		if _, err = d.blockRecords(schema, body); err != nil {
			return true, false, nil //nolint:nilerr // a block that does not read is what Drop removes
		}
	}
	return true, true, nil
}

func segmentBlocks(ctx context.Context, tx sqlite.Writer, id int64) ([][]byte, error) {
	var blocks [][]byte
	rows, err := tx.QueryContext(ctx, selectBlocksToDrop, id) //nolint:rowserrcheck // EachRow checks Err
	if err != nil {
		return nil, err
	}
	err = sqlite.EachRow(rows, "blocks to drop", func(rows *sql.Rows) error {
		var body []byte
		scanErr := rows.Scan(&body)
		blocks = append(blocks, body)
		return scanErr
	})
	return blocks, err
}
