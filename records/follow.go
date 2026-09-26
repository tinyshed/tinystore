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
	release, err := s.admitTo(ctx, s.reads)
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
		return Batch{}, s.noted(err)
	}

	batch, err := s.buildBatch(ctx, after, limit, followed)
	if err != nil {
		return Batch{}, s.noted(err)
	}
	s.countRead(followed.blocks, followed.bytes)
	return batch, nil
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
// the places from the cursor on, each with the blocks of its holder the batch
// needs
type followed struct {
	places   []followedPlace
	sequence int64 // the last segment id ever given, when no place was found
	blocks   int
	bytes    int
}

// followedPlace is one place in the order segments were sealed: its records
// are count of its holder's, from start on
type followedPlace struct {
	id, stream int64
	count      int
	start      int
	holder     *heldSegment
	skip       int // rows the cursor has passed at this place
	first      int // the holder's records before the first block fetched
	blocks     []followedBlock
}

// heldSegment is a segment that holds records, its own place's and those of
// the places merged into it, with what one batch fetched and decoded of it
type heldSegment struct {
	id, stream            int64
	first, last           int64
	firstBlock, lastBlock int64
	row                   []byte
	listed                []followedBlock
	schema                *schema
	decoded               map[int64][]Record
}

type followedBlock struct {
	id    int64
	count int
	body  []byte
}

const (
	selectFollowedPlaces = `
		select id, stream, count, coalesce(holder, id), start from segments
		where id >= ?
		order by id
		limit cast(? as integer)`
	selectHolder = `
		select stream, first_at, last_at, first_block, last_block, body from segments
		where id = ? and holder is null`
	selectFollowedBlocks  = `select id, count from blocks where id between ? and ? order by id`
	selectSegmentSequence = `select coalesce(max(seq), 0) from sqlite_sequence where name = 'segments'`
)

func (s *Store) fetchFollowed(ctx context.Context, after Cursor, limit int) (followed, error) {
	ctx, cancel := context.WithTimeout(ctx, snapshotTimeout)
	defer cancel()
	var out followed
	err := s.file.ViewPrepared(ctx, func(tx sqlite.Reader) error {
		read := followRead{tx: tx, after: after, limit: limit, budget: s.opts.Budget.Bytes, names: &s.streams}
		var err error
		out.places, err = read.places(ctx)
		out.blocks, out.bytes = read.fetched, read.spent
		if err != nil || len(out.places) > 0 {
			return err
		}
		return sqlite.QueryRow(ctx, tx, selectSegmentSequence).Scan(&out.sequence)
	})
	if err != nil {
		return followed{}, fmt.Errorf("records: follow: %w", err)
	}
	return out, nil
}

// followRead gathers places and the blocks holding them until the batch has
// limit records or has spent its bytes
type followRead struct {
	tx       sqlite.Reader
	after    Cursor
	limit    int
	budget   int
	names    *streams
	holders  map[int64]*heldSegment
	gathered int
	fetched  int
	spent    int
}

func (r *followRead) places(ctx context.Context) ([]followedPlace, error) {
	//nolint:rowserrcheck // EachRow checks Err
	rows, err := r.tx.QueryContext(ctx, selectFollowedPlaces, max(r.after.Segment, 1), r.limit)
	if err != nil {
		return nil, err
	}
	var found []followedPlace
	var holders []int64
	err = sqlite.EachRow(rows, "followed places", func(rows *sql.Rows) error {
		var place followedPlace
		var holder int64
		scanErr := rows.Scan(&place.id, &place.stream, &place.count, &holder, &place.start)
		found, holders = append(found, place), append(holders, holder)
		return scanErr
	})
	if err != nil {
		return nil, err
	}
	return r.withBlocks(ctx, found, holders)
}

// withBlocks fetches, place by place, the holder's blocks holding the rows
// the batch returns; a holder is fetched once a batch
func (r *followRead) withBlocks(ctx context.Context, found []followedPlace, holders []int64) (
	[]followedPlace, error,
) {
	r.holders = map[int64]*heldSegment{}
	var taken []followedPlace
	for i := range found {
		place := &found[i]
		if place.id == r.after.Segment {
			place.skip = r.after.Row
		}
		if place.skip >= place.count {
			taken = append(taken, *place)
			continue
		}
		if r.gathered >= r.limit {
			break
		}
		holder, affordable, err := r.holder(ctx, place, holders[i], len(taken) > 0)
		if err != nil || !affordable {
			return taken, err
		}
		place.holder = holder
		if err = r.placeBlocks(ctx, place); err != nil {
			return nil, err
		}
		taken = append(taken, *place)
	}
	return taken, nil
}

// holder is the segment holding a place's records, fetched once a batch, and
// whether the batch's bytes afford it when the batch has records already
func (r *followRead) holder(ctx context.Context, place *followedPlace, id int64, taken bool) (
	*heldSegment, bool, error,
) {
	if holder, ok := r.holders[id]; ok {
		return holder, true, nil
	}
	holder := &heldSegment{id: id, decoded: map[int64][]Record{}}
	err := sqlite.QueryRow(ctx, r.tx, selectHolder, id).Scan(&holder.stream, &holder.first, &holder.last,
		&holder.firstBlock, &holder.lastBlock, &holder.row)
	if errors.Is(err, sql.ErrNoRows) {
		found := Damage{Stream: r.names.name(place.stream), Segment: place.id}
		gone := corrupt(fmt.Sprintf("place %d names segment %d, which is gone", place.id, id))
		return nil, false, damageOf(found, gone)
	}
	if err != nil || (taken && r.spent+len(holder.row) > r.budget) {
		return nil, false, err
	}
	r.spent += len(holder.row)
	r.holders[id] = holder
	return holder, true, r.listBlocks(ctx, holder)
}

func (r *followRead) listBlocks(ctx context.Context, holder *heldSegment) error {
	//nolint:rowserrcheck // EachRow checks Err
	rows, err := r.tx.QueryContext(ctx, selectFollowedBlocks, holder.firstBlock, holder.lastBlock)
	if err != nil {
		return err
	}
	return sqlite.EachRow(rows, "followed blocks", func(rows *sql.Rows) error {
		var block followedBlock
		scanErr := rows.Scan(&block.id, &block.count)
		holder.listed = append(holder.listed, block)
		return scanErr
	})
}

// placeBlocks fetches the holder's blocks that hold the place's rows past the
// cursor, each block's bytes once a batch
func (r *followRead) placeBlocks(ctx context.Context, place *followedPlace) error {
	from, to := place.start+place.skip, place.start+place.count
	passed := 0
	for i := range place.holder.listed {
		block := &place.holder.listed[i]
		if passed+block.count <= from {
			passed += block.count
			place.first = passed
			continue
		}
		if passed >= to || r.gathered >= r.limit || (r.gathered > 0 && r.spent >= r.budget) {
			break
		}
		if block.body == nil {
			if err := sqlite.QueryRow(ctx, r.tx, selectBlockBody, block.id).Scan(&block.body); err != nil {
				return err
			}
			r.spent += len(block.body)
			r.fetched++
		}
		r.gathered += min(to, passed+block.count) - max(from, passed)
		passed += block.count
		place.blocks = append(place.blocks, *block)
	}
	return nil
}

// buildBatch decodes outside the snapshot and returns the records past the
// cursor, and the cursor after the last one
func (s *Store) buildBatch(ctx context.Context, after Cursor, limit int, f followed) (Batch, error) {
	batch := Batch{Next: after, Expired: expiredBefore(after, f)}
	if len(f.places) == 0 {
		if f.sequence >= after.Segment {
			batch.Next = Cursor{Segment: f.sequence + 1}
		}
		return batch, nil
	}
	d := newDecoder(s.unpack)
	for i := range f.places {
		if err := ctx.Err(); err != nil {
			return Batch{}, err
		}
		place := &f.places[i]
		next, err := s.followPlace(d, place, limit, &batch)
		if err != nil {
			holder := place.holder
			found := Damage{
				Stream: s.streams.name(holder.stream), Segment: holder.id,
				From: timeOf(holder.first), To: timeOf(holder.last),
			}
			return Batch{}, fmt.Errorf("records: follow: %w", damageOf(found, err))
		}
		batch.Next = next
		if len(batch.Records) >= limit {
			break
		}
	}
	return batch, nil
}

// expiredBefore counts the places between the cursor and the first one
// found: ids only grow, and only retention and Drop remove a place
func expiredBefore(after Cursor, f followed) int {
	if after.Segment == 0 {
		return 0
	}
	if len(f.places) == 0 {
		return int(max(0, f.sequence-after.Segment+1))
	}
	return int(max(0, f.places[0].id-after.Segment))
}

// followPlace adds a place's records past the cursor, and returns the cursor
// after the last one it added
func (s *Store) followPlace(d *decoder, place *followedPlace, limit int, batch *Batch) (Cursor, error) {
	if place.skip >= place.count {
		return Cursor{Segment: place.id + 1}, nil
	}
	schema, err := place.holder.parse(d, s.streams.name(place.holder.stream))
	if err != nil {
		return Cursor{}, err
	}
	at, end, passed := place.start+place.skip, place.start+place.count, place.first
	for _, block := range place.blocks {
		records, err := place.holder.decode(d, schema, block)
		if err != nil {
			return Cursor{}, err
		}
		for ; at < end && at-passed < len(records); at++ {
			if len(batch.Records) == limit {
				return Cursor{Segment: place.id, Row: at - place.start}, nil
			}
			batch.Records = append(batch.Records, records[at-passed])
		}
		passed += len(records)
	}
	if at >= end {
		return Cursor{Segment: place.id + 1}, nil
	}
	return Cursor{Segment: place.id, Row: at - place.start}, nil
}

// parse reads the holder's row once a batch
func (h *heldSegment) parse(d *decoder, stream string) (*schema, error) {
	if h.schema != nil {
		return h.schema, nil
	}
	parsed, err := d.parseSchema(h.row)
	if err != nil {
		return nil, err
	}
	if parsed.stream != stream {
		return nil, corrupt("segment row names another stream")
	}
	h.schema = parsed
	return parsed, nil
}

// decode reads one of the holder's blocks once a batch, however many places it holds
func (h *heldSegment) decode(d *decoder, s *schema, block followedBlock) ([]Record, error) {
	if records, ok := h.decoded[block.id]; ok {
		return records, nil
	}
	records, err := d.blockRecords(s, block.body)
	if err != nil {
		return nil, errors.Join(fmt.Errorf("block %d", block.id), err)
	}
	h.decoded[block.id] = records
	return records, nil
}

func (d *decoder) blockRecords(s *schema, body []byte) ([]Record, error) {
	block, err := d.openBlock(s, body)
	if err != nil {
		return nil, err
	}
	return d.records(block, nil)
}
