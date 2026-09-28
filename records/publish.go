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
	s.appendMu.Lock()
	defer s.appendMu.Unlock()
	emptied := false
	err := s.file.UpdatePrepared(ctx, func(tx sqlite.Writer) error {
		owner, err := insertSegmentRow(ctx, tx, head.stream, &segment)
		if err != nil {
			return err
		}
		if err = insertBlocks(ctx, tx, owner, segment.blocks); err != nil {
			return err
		}
		if err = insertKeys(ctx, tx, owner.segment, segment.keys); err != nil {
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

const selectNextBlockID = `select coalesce(max(seq), 0) + 1 from sqlite_sequence where name = 'blocks'`

// nextBlockID is the id a segment's first block takes: the writer is alone,
// so a segment's blocks are the ids from it on, named by the segment row; the
// sequence remembers the ids of deleted blocks, so none is given twice
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

// insertSegmentRow writes a sealed segment's row, which names the blocks it
// is about to be given, and returns their owner
func insertSegmentRow(
	ctx context.Context, tx sqlite.Writer, stream int64, segment *encodedSegment,
) (blockOwner, error) {
	firstBlock, err := nextBlockID(ctx, tx)
	if err != nil {
		return blockOwner{}, err
	}
	owner := blockOwner{stream: stream, firstBlock: firstBlock}
	err = sqlite.QueryRow(ctx, tx, insertSegment, stream, segment.first, segment.last, segment.count,
		segment.count, segment.input, firstBlock, owner.lastBlock(segment.blocks), segment.row).Scan(&owner.segment)
	return owner, err
}

// blockOwner is the segment new blocks belong to, and the id the first of them takes
type blockOwner struct {
	segment, stream, firstBlock int64
}

func (o blockOwner) lastBlock(blocks []encodedBlock) int64 {
	return o.firstBlock + int64(len(blocks)) - 1
}

const (
	insertBlock = `
		insert into blocks (id, segment, stream, first_at, last_at, span, levels, count, size, body)
		values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`
	insertTraces = `insert into block_traces (block, bloom) values (?, ?)`
	insertFilter = `insert into block_filters (block, key, bloom) values (?, ?, ?)`
)

func insertBlocks(ctx context.Context, tx sqlite.Writer, owner blockOwner, blocks []encodedBlock) error {
	for i, block := range blocks {
		id := owner.firstBlock + int64(i)
		_, err := tx.ExecContext(ctx, insertBlock, id, owner.segment, owner.stream, block.first, block.last,
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
	return settleHead(ctx, tx, head, chunk.headWeight)
}

// headWeight is what rows take from their head's state: their records and their input
type headWeight struct {
	count, input int
}

const (
	subtractFromHead = `update head_state set count = count - ?, input = input - ? where stream = ? and late = ?`
	selectOldestHead = `select written_at from heads where stream = ? and late = ? order by id limit 1`
	setHeadSince     = `update head_state set since = ? where stream = ? and late = ?`
	deleteHeadState  = `delete from head_state where stream = ? and late = ?`
)

// settleHead takes what rows left a head off its state, then dates the state
// by the oldest row left, or removes it and says so when none is
func settleHead(ctx context.Context, tx sqlite.Writer, head headKey, left headWeight) (emptied bool, err error) {
	if _, err = tx.ExecContext(ctx, subtractFromHead, left.count, left.input, head.stream, head.late); err != nil {
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
