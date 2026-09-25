package records

import (
	"context"
	"database/sql"
	"errors"
	"fmt"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Follow returns up to limit records of the sealed segments from after on:
// segments in the order they were sealed, each one's records in event-time
// order, and the cursor to continue from. A record reaches Follow once its
// head is sealed, a segment's worth or SealAge after it arrived; Read sees it
// at once. A late record is in a later segment than its neighbours in time.
func (s *Store) Follow(ctx context.Context, after Cursor, limit int) (Batch, error) {
	release, err := s.admit(ctx)
	if err != nil {
		return Batch{}, err
	}
	defer release()

	if limit, err = checkFollow(after, limit); err != nil {
		return Batch{}, err
	}

	unreserve, err := s.reserve(ctx, func() int64 { return s.followReservation(limit) })
	if err != nil {
		return Batch{}, err
	}
	defer unreserve()

	followed, err := s.fetchFollowed(ctx, after, limit)
	if err != nil {
		return Batch{}, err
	}

	batch, err := s.buildBatch(ctx, after, limit, followed)
	if err == nil {
		s.countRead(followed.blocks, followed.bytes)
	}
	return batch, err
}

func checkFollow(after Cursor, limit int) (int, error) {
	if after.Segment < 0 || after.Row < 0 {
		return 0, fmt.Errorf("%w: cursor %+v", tinystore.ErrInvalid, after)
	}
	return checkLimit(limit)
}

func (s *Store) followReservation(limit int) int64 {
	return int64(s.opts.Budget.Bytes) + blockReservation + int64(limit)*recordReservation
}

// followed is what one batch decodes, copied out of one read transaction:
// the segments from the cursor on, each with the blocks the batch needs
type followed struct {
	segments []followedSegment
	sequence int64 // the last segment id ever given, when no segment was found
	blocks   int
	bytes    int
}

type followedSegment struct {
	id, stream int64
	count      int
	row        []byte
	skip       int // rows the cursor has passed in this segment
	start      int // rows before the first block fetched
	blocks     []followedBlock
}

type followedBlock struct {
	id    int64
	count int
	body  []byte
}

const (
	selectFollowedSegments = `
		select id, stream, count, first_block, last_block, body from segments
		where id >= ?
		order by id
		limit cast(? as integer)`
	selectFollowedBlocks  = `select id, count from blocks where id between ? and ? order by id`
	selectSegmentSequence = `select coalesce(max(seq), 0) from sqlite_sequence where name = 'segments'`
)

func (s *Store) fetchFollowed(ctx context.Context, after Cursor, limit int) (followed, error) {
	ctx, cancel := context.WithTimeout(ctx, snapshotTimeout)
	defer cancel()
	var out followed
	err := s.file.ViewPrepared(ctx, func(tx sqlite.Reader) error {
		read := followRead{tx: tx, after: after, limit: limit, budget: s.opts.Budget.Bytes}
		var err error
		out.segments, err = read.segments(ctx)
		out.blocks, out.bytes = read.fetched, read.spent
		if err != nil || len(out.segments) > 0 {
			return err
		}
		return sqlite.QueryRow(ctx, tx, selectSegmentSequence).Scan(&out.sequence)
	})
	if err != nil {
		return followed{}, fmt.Errorf("records: follow: %w", err)
	}
	return out, nil
}

// followRead gathers segments and their blocks until the batch has limit
// records or has spent its bytes
type followRead struct {
	tx       sqlite.Reader
	after    Cursor
	limit    int
	budget   int
	gathered int
	fetched  int
	spent    int
}

func (r *followRead) segments(ctx context.Context) ([]followedSegment, error) {
	//nolint:rowserrcheck // EachRow checks Err
	rows, err := r.tx.QueryContext(ctx, selectFollowedSegments, max(r.after.Segment, 1), r.limit)
	if err != nil {
		return nil, err
	}
	var found []followedSegment
	var blocks [][2]int64
	err = sqlite.EachRow(rows, "followed segments", func(rows *sql.Rows) error {
		var segment followedSegment
		var first, last int64
		scanErr := rows.Scan(&segment.id, &segment.stream, &segment.count, &first, &last, &segment.row)
		found, blocks = append(found, segment), append(blocks, [2]int64{first, last})
		return scanErr
	})
	if err != nil {
		return nil, err
	}
	return r.withBlocks(ctx, found, blocks)
}

// withBlocks fetches, segment by segment, the blocks holding the rows the batch returns
func (r *followRead) withBlocks(ctx context.Context, found []followedSegment, blocks [][2]int64) (
	[]followedSegment, error,
) {
	var taken []followedSegment
	for i := range found {
		segment := &found[i]
		if segment.id == r.after.Segment {
			segment.skip = r.after.Row
		}
		if segment.skip >= segment.count {
			taken = append(taken, *segment)
			continue
		}
		if r.gathered >= r.limit || (len(taken) > 0 && r.spent+len(segment.row) > r.budget) {
			break
		}
		r.spent += len(segment.row)
		if err := r.segmentBlocks(ctx, segment, blocks[i]); err != nil {
			return nil, err
		}
		taken = append(taken, *segment)
	}
	return taken, nil
}

func (r *followRead) segmentBlocks(ctx context.Context, segment *followedSegment, ids [2]int64) error {
	//nolint:rowserrcheck // EachRow checks Err
	rows, err := r.tx.QueryContext(ctx, selectFollowedBlocks, ids[0], ids[1])
	if err != nil {
		return err
	}
	var listed []followedBlock
	err = sqlite.EachRow(rows, "followed blocks", func(rows *sql.Rows) error {
		var block followedBlock
		scanErr := rows.Scan(&block.id, &block.count)
		listed = append(listed, block)
		return scanErr
	})
	if err != nil {
		return err
	}
	passed := 0
	for _, block := range listed {
		if passed+block.count <= segment.skip {
			passed += block.count
			segment.start = passed
			continue
		}
		if r.gathered >= r.limit || (r.gathered > 0 && r.spent >= r.budget) {
			break
		}
		if err = sqlite.QueryRow(ctx, r.tx, selectBlockBody, block.id).Scan(&block.body); err != nil {
			return err
		}
		r.spent += len(block.body)
		r.fetched++
		r.gathered += block.count - max(0, segment.skip-passed)
		passed += block.count
		segment.blocks = append(segment.blocks, block)
	}
	return nil
}

// buildBatch decodes outside the snapshot and returns the records past the
// cursor, and the cursor after the last one
func (s *Store) buildBatch(ctx context.Context, after Cursor, limit int, f followed) (Batch, error) {
	batch := Batch{Next: after, Expired: expiredBefore(after, f)}
	if len(f.segments) == 0 {
		if f.sequence >= after.Segment {
			batch.Next = Cursor{Segment: f.sequence + 1}
		}
		return batch, nil
	}
	d := newDecoder(s.unpack)
	for i := range f.segments {
		if err := ctx.Err(); err != nil {
			return Batch{}, err
		}
		next, err := s.followSegment(d, &f.segments[i], limit, &batch)
		if err != nil {
			return Batch{}, fmt.Errorf("records: follow segment %d: %w", f.segments[i].id, err)
		}
		batch.Next = next
		if len(batch.Records) >= limit {
			break
		}
	}
	return batch, nil
}

// expiredBefore counts the segments between the cursor and the first one
// found: ids only grow and only retention removes a segment
func expiredBefore(after Cursor, f followed) int {
	if after.Segment == 0 {
		return 0
	}
	if len(f.segments) == 0 {
		return int(max(0, f.sequence-after.Segment+1))
	}
	return int(max(0, f.segments[0].id-after.Segment))
}

// followSegment adds a segment's records past the cursor, and returns the
// cursor after the last one it added
func (s *Store) followSegment(d *decoder, segment *followedSegment, limit int, batch *Batch) (Cursor, error) {
	schema, err := d.parseSchema(segment.row)
	if err != nil {
		return Cursor{}, err
	}
	if schema.stream != s.streams.name(segment.stream) {
		return Cursor{}, corrupt("segment row names another stream")
	}
	row, passed := segment.skip, segment.start
	for _, block := range segment.blocks {
		records, err := d.blockRecords(schema, block.body)
		if err != nil {
			return Cursor{}, errors.Join(fmt.Errorf("block %d", block.id), err)
		}
		for _, record := range records[max(0, row-passed):] {
			if len(batch.Records) == limit {
				return Cursor{Segment: segment.id, Row: row}, nil
			}
			batch.Records = append(batch.Records, record)
			row++
		}
		passed += len(records)
	}
	if row >= segment.count {
		return Cursor{Segment: segment.id + 1}, nil
	}
	return Cursor{Segment: segment.id, Row: row}, nil
}

func (d *decoder) blockRecords(s *schema, body []byte) ([]Record, error) {
	block, err := d.openBlock(s, body)
	if err != nil {
		return nil, err
	}
	return d.records(block, nil)
}
