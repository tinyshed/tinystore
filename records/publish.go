package records

import (
	"context"
	"database/sql"
	"errors"
	"fmt"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// publish writes a segment with its blocks, filters and keys, and deletes the
// head rows it was made of, in one transaction: a reader finds each record in
// the head or in the segment, never in both and never in neither
func (s *Store) publish(ctx context.Context, head headKey, chunk headChunk, segment encodedSegment) error {
	s.spans.note(segment.blocks)
	emptied := false
	err := s.file.UpdatePrepared(ctx, func(tx sqlite.Writer) error {
		firstBlock, err := nextBlockID(ctx, tx)
		if err != nil {
			return err
		}
		id, err := insertSegmentRow(ctx, tx, head.stream, &segment, firstBlock)
		if err != nil {
			return err
		}
		place := blockPlace{segment: id, stream: head.stream, first: firstBlock}
		if err = insertBlocks(ctx, tx, place, segment.blocks); err != nil {
			return err
		}
		if err = insertKeys(ctx, tx, id, segment.keys); err != nil {
			return err
		}
		emptied, err = removeHeadRows(ctx, tx, head, chunk)
		return err
	})
	if err != nil {
		return fmt.Errorf("records: publish a segment of %s: %w", segment.stream, err)
	}
	if emptied && !head.late {
		s.waiting.forget(segment.stream)
	}
	return nil
}

const selectNextBlockID = `select coalesce(max(id), 0) + 1 from blocks`

// nextBlockID is the id a segment's first block takes: the writer is alone,
// so a segment's blocks are the ids from it on, named by the segment row
func nextBlockID(ctx context.Context, tx sqlite.Writer) (int64, error) {
	var id int64
	err := sqlite.QueryRow(ctx, tx, selectNextBlockID).Scan(&id)
	return id, err
}

// a sealed segment holds its own records, and they begin its blocks
const insertSegment = `
	insert into segments (stream, first_at, last_at, count, start, held, input, first_block, last_block, body)
	values (?, ?, ?, ?, 0, ?, ?, ?, ?, ?)
	returning id`

func insertSegmentRow(
	ctx context.Context, tx sqlite.Writer, stream int64, segment *encodedSegment, firstBlock int64,
) (int64, error) {
	lastBlock := firstBlock + int64(len(segment.blocks)) - 1
	var id int64
	err := sqlite.QueryRow(ctx, tx, insertSegment, stream, segment.first, segment.last, segment.count,
		segment.count, segment.input, firstBlock, lastBlock, segment.row).Scan(&id)
	return id, err
}

// blockPlace is where a segment's blocks go: its id, its stream, the first block id
type blockPlace struct {
	segment, stream, first int64
}

const (
	insertBlock = `
		insert into blocks (id, segment, stream, first_at, last_at, span, levels, count, size, body)
		values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`
	insertTraces = `insert into block_traces (block, bloom) values (?, ?)`
	insertFilter = `insert into block_filters (block, key, bloom) values (?, ?, ?)`
)

func insertBlocks(ctx context.Context, tx sqlite.Writer, place blockPlace, blocks []encodedBlock) error {
	for i, block := range blocks {
		id := place.first + int64(i)
		_, err := tx.ExecContext(ctx, insertBlock, id, place.segment, place.stream, block.first, block.last,
			spanOf(block.first, block.last), block.levels, block.count, len(block.body), block.body)
		if err != nil {
			return err
		}
		if block.traces != nil {
			if _, err = tx.ExecContext(ctx, insertTraces, id, block.traces); err != nil {
				return err
			}
		}
		for _, filter := range block.filters {
			if _, err = tx.ExecContext(ctx, insertFilter, id, filter.key, filter.bloom); err != nil {
				return err
			}
		}
	}
	return nil
}

const insertKey = `insert into segment_keys (segment, kind, key) values (?, ?, ?)`

func insertKeys(ctx context.Context, tx sqlite.Writer, segment int64, keys []segmentKey) error {
	for _, key := range keys {
		if _, err := tx.ExecContext(ctx, insertKey, segment, key.kind, key.key); err != nil {
			return err
		}
	}
	return nil
}

const deleteHeadRow = `delete from heads where id = ?`

// removeHeadRows deletes what a segment took from its head, and says whether
// the head is empty now
func removeHeadRows(ctx context.Context, tx sqlite.Writer, head headKey, chunk headChunk) (bool, error) {
	for _, id := range chunk.ids {
		if _, err := tx.ExecContext(ctx, deleteHeadRow, id); err != nil {
			return false, err
		}
	}
	return settleHead(ctx, tx, head, chunk.count, chunk.input)
}

const (
	subtractFromHead = `update head_state set count = count - ?, input = input - ? where stream = ? and late = ?`
	selectOldestHead = `select written_at from heads where stream = ? and late = ? order by id limit 1`
	setHeadSince     = `update head_state set since = ? where stream = ? and late = ?`
	deleteHeadState  = `delete from head_state where stream = ? and late = ?`
)

// settleHead takes what left a head off its state, then dates the state by
// the oldest row left, or removes it and says so when none is
func settleHead(ctx context.Context, tx sqlite.Writer, head headKey, count, input int) (emptied bool, err error) {
	if _, err = tx.ExecContext(ctx, subtractFromHead, count, input, head.stream, head.late); err != nil {
		return false, err
	}
	var since int64
	err = sqlite.QueryRow(ctx, tx, selectOldestHead, head.stream, head.late).Scan(&since)
	if errors.Is(err, sql.ErrNoRows) {
		_, err = tx.ExecContext(ctx, deleteHeadState, head.stream, head.late)
		return err == nil, err
	}
	if err != nil {
		return false, err
	}
	_, err = tx.ExecContext(ctx, setHeadSince, since, head.stream, head.late)
	return false, err
}
