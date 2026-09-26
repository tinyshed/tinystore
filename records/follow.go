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

// followReservation is what a Follow holds: its budget, a decoded block, its records, and the
// cache it fills for the batches after it
func (s *Store) followReservation(limit int) int64 {
	return int64(s.opts.Budget.Bytes) + blockReservation + int64(limit)*recordReservation + followCacheBytes
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

// followedPlace is one place in the order segments were sealed; its records
// are the count rows of its holder that begin at row start
type followedPlace struct {
	id, stream  int64
	count       int
	start       int
	holderID    int64
	holder      *heldSegment
	skip        int // rows the cursor has passed at this place
	fetchedFrom int // the holder's row the first fetched block begins at
	blocks      []followedBlock
}

// heldSegment is a segment that holds records, its own and those of the
// places merged into it, and what one batch fetched and decoded of it
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
		select stream, first_at, last_at, first_block, last_block from segments
		where id = ? and holder is null`
	selectHolderRow       = `select body from segments where id = ?`
	selectFollowedBlocks  = `select id, count from blocks where id between ? and ? order by id`
	selectSegmentSequence = `select coalesce(max(seq), 0) from sqlite_sequence where name = 'segments'`
)

func (s *Store) fetchFollowed(ctx context.Context, after Cursor, limit int) (followed, error) {
	ctx, cancel := context.WithTimeout(ctx, snapshotTimeout)
	defer cancel()
	var out followed
	err := s.file.ViewPrepared(ctx, func(tx sqlite.Reader) error {
		read := followRead{
			tx: tx, after: after, limit: limit, budget: s.opts.Budget.Bytes, names: &s.streams, cache: &s.cache,
		}
		var err error
		out.places, err = read.places(ctx)
		out.blocks, out.bytes = read.fetched, read.fetchedBytes
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
// limit records or has spent its bytes; a byte the cache had is spent as one
// fetched from the file, so that a batch ends where it would without the cache
type followRead struct {
	tx           sqlite.Reader
	after        Cursor
	limit        int
	budget       int
	names        *streams
	cache        *followCache
	holders      map[int64]*heldSegment
	taken        []followedPlace
	gathered     int
	spent        int
	fetched      int // blocks read from the file
	fetchedBytes int // bytes read from the file
}

func (r *followRead) places(ctx context.Context) ([]followedPlace, error) {
	//nolint:rowserrcheck // EachRow checks Err
	rows, err := r.tx.QueryContext(ctx, selectFollowedPlaces, max(r.after.Segment, 1), r.limit)
	if err != nil {
		return nil, err
	}
	var found []followedPlace
	err = sqlite.EachRow(rows, "followed places", func(rows *sql.Rows) error {
		var place followedPlace
		scanErr := rows.Scan(&place.id, &place.stream, &place.count, &place.holderID, &place.start)
		found = append(found, place)
		return scanErr
	})
	if err != nil {
		return nil, err
	}
	return r.withBlocks(ctx, found)
}

// withBlocks takes places one after another with the holder's blocks holding
// the rows the batch returns; a holder is fetched once a batch
func (r *followRead) withBlocks(ctx context.Context, found []followedPlace) ([]followedPlace, error) {
	r.holders = map[int64]*heldSegment{}
	for i := range found {
		place := &found[i]
		if place.id == r.after.Segment {
			place.skip = r.after.Row
		}
		if place.skip >= place.count {
			r.taken = append(r.taken, *place)
			continue
		}
		if r.gathered >= r.limit {
			break
		}
		holder, err := r.holderOf(ctx, place)
		if err != nil || holder == nil {
			return r.taken, err
		}
		place.holder = holder
		if err = r.placeBlocks(ctx, place); err != nil {
			return nil, err
		}
		r.taken = append(r.taken, *place)
	}
	return r.taken, nil
}

// holderOf is the segment holding a place's records, fetched once a batch;
// nil when the batch has taken places already and its bytes do not afford
// another holder's row
func (r *followRead) holderOf(ctx context.Context, place *followedPlace) (*heldSegment, error) {
	if holder, ok := r.holders[place.holderID]; ok {
		return holder, nil
	}
	holder := &heldSegment{id: place.holderID, decoded: map[int64][]Record{}}
	err := sqlite.QueryRow(ctx, r.tx, selectHolder, holder.id).Scan(&holder.stream, &holder.first, &holder.last,
		&holder.firstBlock, &holder.lastBlock)
	if errors.Is(err, sql.ErrNoRows) {
		found := Damage{Stream: r.names.name(place.stream), Segment: place.id}
		gone := corrupt(fmt.Sprintf("place %d names segment %d, which is gone", place.id, holder.id))
		return nil, damageOf(found, gone)
	}
	if err == nil {
		holder.row, _, err = r.fetch(ctx, rowCacheKey(holder), selectHolderRow, holder.id)
	}
	if err != nil || (len(r.taken) > 0 && r.spent+len(holder.row) > r.budget) {
		return nil, err
	}
	r.spent += len(holder.row)
	r.holders[holder.id] = holder
	return holder, r.listBlocks(ctx, holder)
}

// fetch is a block's or a segment row's bytes, from the cache or else from
// the file, and whether the file was read
func (r *followRead) fetch(ctx context.Context, key cacheKey, query string, id int64) ([]byte, bool, error) {
	if body, ok := r.cache.get(key); ok {
		return body, false, nil
	}
	var body []byte
	if err := sqlite.QueryRow(ctx, r.tx, query, id).Scan(&body); err != nil {
		return nil, false, err
	}
	r.fetchedBytes += len(body)
	r.cache.put(key, body)
	return body, true, nil
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
	blockStart := 0
	for i := range place.holder.listed {
		block := &place.holder.listed[i]
		if blockStart+block.count <= from {
			blockStart += block.count
			place.fetchedFrom = blockStart
			continue
		}
		if blockStart >= to || r.gathered >= r.limit || (r.gathered > 0 && r.spent >= r.budget) {
			break
		}
		if block.body == nil {
			body, read, err := r.fetch(ctx, blockCacheKey(block.id), selectBlockBody, block.id)
			if err != nil {
				return err
			}
			if read {
				r.fetched++
			}
			block.body = body
			r.spent += len(body)
		}
		r.gathered += min(to, blockStart+block.count) - max(from, blockStart)
		blockStart += block.count
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
	next, end := place.start+place.skip, place.start+place.count
	blockStart := place.fetchedFrom
	for _, block := range place.blocks {
		records, err := place.holder.decode(d, schema, block)
		if err != nil {
			return Cursor{}, err
		}
		for ; next < end && next-blockStart < len(records); next++ {
			if len(batch.Records) == limit {
				return Cursor{Segment: place.id, Row: next - place.start}, nil
			}
			batch.Records = append(batch.Records, records[next-blockStart])
		}
		blockStart += len(records)
	}
	if next >= end {
		return Cursor{Segment: place.id + 1}, nil
	}
	return Cursor{Segment: place.id, Row: next - place.start}, nil
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
