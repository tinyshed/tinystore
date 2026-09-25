package spike

import (
	"bytes"
	"cmp"
	"context"
	"database/sql"
	"fmt"
	"os"
	"slices"
)

// segments hold what their blocks share; a query reads the covering index first,
// then only the block bodies it cannot rule out
const v2StoreSchema = `create table segments (id integer primary key, stream text not null,
	first_at integer not null, last_at integer not null, count integer not null, body blob not null) strict;
create table blocks (id integer primary key, segment integer not null, first_at integer not null,
	last_at integer not null, count integer not null, levels integer not null, body blob not null) strict;
create index blocks_time on blocks (first_at, last_at, levels);
create table block_traces (block integer primary key, bloom blob not null) strict;`

const (
	v2BloomBits   = 10
	v2BloomHashes = 7
)

func v2BloomHash(id []byte) (uint64, uint64) {
	hash := uint64(14695981039346656037)
	for _, b := range id {
		hash = (hash ^ uint64(b)) * 1099511628211
	}
	mixed := (hash ^ hash>>31) * 0x9e3779b97f4a7c15
	return hash, mixed | 1
}

func v2Bloom(flat []byte, width int) []byte {
	distinct := map[string]bool{}
	for start := 0; start < len(flat); start += width {
		distinct[string(flat[start:start+width])] = true
	}
	filter := make([]byte, max(8, (len(distinct)*v2BloomBits+7)/8))
	bits := uint64(len(filter)) * 8
	for id := range distinct {
		a, b := v2BloomHash([]byte(id))
		for i := range uint64(v2BloomHashes) {
			bit := (a + i*b) % bits
			filter[bit/8] |= 1 << (bit % 8)
		}
	}
	return filter
}

func v2BloomMayHold(filter, id []byte) bool {
	bits := uint64(len(filter)) * 8
	a, b := v2BloomHash(id)
	for i := range uint64(v2BloomHashes) {
		if bit := (a + i*b) % bits; filter[bit/8]&(1<<(bit%8)) == 0 {
			return false
		}
	}
	return true
}

const (
	v2InsertSegment = `insert into segments (stream, first_at, last_at, count, body) values (?, ?, ?, ?, ?)`
	v2InsertBlock   = `insert into blocks (segment, first_at, last_at, count, levels, body) values (?, ?, ?, ?, ?, ?)`
	v2InsertBloom   = `insert into block_traces (block, bloom) values (?, ?)`
)

func v2WriteSegments(ctx context.Context, db *sql.DB, segments []v2Segment) error {
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	for i := range segments {
		if err = v2WriteSegment(ctx, tx, &segments[i]); err != nil {
			return err
		}
	}
	return tx.Commit()
}

func v2WriteSegment(ctx context.Context, tx *sql.Tx, segment *v2Segment) error {
	result, err := tx.ExecContext(ctx, v2InsertSegment, segment.schema.stream, segment.first, segment.last,
		segment.schema.count, segment.row)
	if err != nil {
		return err
	}
	id, err := result.LastInsertId()
	if err != nil {
		return err
	}
	for _, block := range segment.blocks {
		result, err = tx.ExecContext(ctx, v2InsertBlock, id, block.first, block.last, block.count, block.levels, block.body)
		if err != nil {
			return err
		}
		if len(block.traces) == 0 {
			continue
		}
		blockID, err := result.LastInsertId()
		if err != nil {
			return err
		}
		if _, err = tx.ExecContext(ctx, v2InsertBloom, blockID, v2Bloom(block.traces, 16)); err != nil {
			return err
		}
	}
	return nil
}

// v2Reader counts what a query had to fetch
type v2Reader struct {
	db       *sql.DB
	decoder  *v2Decoder
	schemas  map[int64]*v2Schema
	segments int
	blocks   int
	bytes    int
	decoded  int
}

type v2Candidate struct {
	id, segment int64
}

const v2SelectRange = `select id, segment from blocks where first_at < ? and last_at >= ?
	and (? = 0 or levels & ? != 0) order by first_at`

func (r *v2Reader) candidates(ctx context.Context, from, to, levels int64) ([]v2Candidate, error) {
	rows, err := r.db.QueryContext(ctx, v2SelectRange, to, from, levels, levels)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var found []v2Candidate
	for rows.Next() {
		var candidate v2Candidate
		if err = rows.Scan(&candidate.id, &candidate.segment); err != nil {
			return nil, err
		}
		found = append(found, candidate)
	}
	return found, rows.Err()
}

func (r *v2Reader) schema(ctx context.Context, segment int64) (*v2Schema, error) {
	if schema, ok := r.schemas[segment]; ok {
		return schema, nil
	}
	var body []byte
	if err := r.db.QueryRowContext(ctx, `select body from segments where id = ?`, segment).Scan(&body); err != nil {
		return nil, err
	}
	schema, err := r.decoder.decodeSchema(body)
	if err != nil {
		return nil, err
	}
	r.segments++
	r.bytes += len(body)
	r.schemas[segment] = &schema
	return &schema, nil
}

func (r *v2Reader) open(ctx context.Context, candidate v2Candidate) (*v2Opened, error) {
	schema, err := r.schema(ctx, candidate.segment)
	if err != nil {
		return nil, err
	}
	var body []byte
	if err = r.db.QueryRowContext(ctx, `select body from blocks where id = ?`, candidate.id).Scan(&body); err != nil {
		return nil, err
	}
	r.blocks++
	r.bytes += len(body)
	return r.decoder.openBlock(schema, body)
}

// v2Match picks the rows of one block a query keeps; nil keeps none
type v2Match func(block *v2Opened) ([]int, error)

func (r *v2Reader) read(ctx context.Context, candidates []v2Candidate, from, to int64, match v2Match) ([]recordEvent, error) {
	var events []recordEvent
	for _, candidate := range candidates {
		block, err := r.open(ctx, candidate)
		if err != nil {
			return nil, err
		}
		rows, err := match(block)
		if err != nil {
			return nil, err
		}
		rows, err = r.inRange(block, rows, from, to)
		if err != nil {
			return nil, err
		}
		if len(rows) == 0 {
			continue
		}
		r.decoded++
		picked, err := r.decoder.events(block, rows)
		if err != nil {
			return nil, err
		}
		events = append(events, picked...)
	}
	slices.SortStableFunc(events, func(a, b recordEvent) int { return cmp.Compare(a.at, b.at) })
	return events, nil
}

func (r *v2Reader) inRange(block *v2Opened, rows []int, from, to int64) ([]int, error) {
	times, err := r.decoder.intSlot(block, 0)
	if err != nil {
		return nil, err
	}
	kept := rows[:0]
	for _, row := range rows {
		if times[row] >= from && times[row] < to {
			kept = append(kept, row)
		}
	}
	return kept, nil
}

func v2AllRows(block *v2Opened) ([]int, error) {
	rows := make([]int, block.count)
	for i := range rows {
		rows[i] = i
	}
	return rows, nil
}

// v2RowsOf maps the values of one slot back to the rows that carry it
func v2RowsOf(block *v2Opened, index int, keep func(value int) bool) []int {
	var rows []int
	value := 0
	for row, shape := range block.shapes {
		if slices.Contains(block.schema.shapeSlots[shape], index) {
			if keep(value) {
				rows = append(rows, row)
			}
			value++
		}
	}
	return rows
}

func (r *v2Reader) readRange(ctx context.Context, from, to int64) ([]recordEvent, error) {
	candidates, err := r.candidates(ctx, from, to, 0)
	if err != nil {
		return nil, err
	}
	return r.read(ctx, candidates, from, to, v2AllRows)
}

func (r *v2Reader) readLevel(ctx context.Context, from, to, minimum int64) ([]recordEvent, error) {
	mask := int64(0)
	for bucket := v2LevelBit(minimum); bucket <= 1<<7; bucket <<= 1 {
		mask |= bucket
	}
	candidates, err := r.candidates(ctx, from, to, mask)
	if err != nil {
		return nil, err
	}
	return r.read(ctx, candidates, from, to, func(block *v2Opened) ([]int, error) {
		index := block.slot(v2SlotLevel, 0)
		levels, err := r.decoder.intSlot(block, index)
		if err != nil {
			return nil, err
		}
		return v2RowsOf(block, index, func(i int) bool { return levels[i] >= minimum }), nil
	})
}

const v2SelectBlooms = `select t.block, b.segment, t.bloom from block_traces t join blocks b on b.id = t.block
	where b.first_at < ? and b.last_at >= ?`

func (r *v2Reader) bloomCandidates(ctx context.Context, trace []byte, from, to int64) ([]v2Candidate, error) {
	rows, err := r.db.QueryContext(ctx, v2SelectBlooms, to, from)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var candidates []v2Candidate
	for rows.Next() {
		var candidate v2Candidate
		var bloom []byte
		if err = rows.Scan(&candidate.id, &candidate.segment, &bloom); err != nil {
			return nil, err
		}
		r.bytes += len(bloom)
		if v2BloomMayHold(bloom, trace) {
			candidates = append(candidates, candidate)
		}
	}
	return candidates, rows.Err()
}

func (r *v2Reader) readTrace(ctx context.Context, trace []byte, from, to int64) ([]recordEvent, error) {
	candidates, err := r.bloomCandidates(ctx, trace, from, to)
	if err != nil {
		return nil, err
	}
	return r.read(ctx, candidates, from, to, func(block *v2Opened) ([]int, error) {
		index := block.slot(v2SlotTrace, 0)
		if index < 0 {
			return nil, nil
		}
		traces, err := r.decoder.bytesSlot(block, index, 16)
		if err != nil {
			return nil, err
		}
		return v2RowsOf(block, index, func(i int) bool { return bytes.Equal(traces[i], trace) }), nil
	})
}

// readContext finds a producer's records through the segment dictionaries, not an index
func (r *v2Reader) readContext(ctx context.Context, field recordField, from, to int64) ([]recordEvent, error) {
	candidates, err := r.candidates(ctx, from, to, 0)
	if err != nil {
		return nil, err
	}
	matches := map[*v2Schema][]bool{}
	var kept []v2Candidate
	for _, candidate := range candidates {
		schema, err := r.schema(ctx, candidate.segment)
		if err != nil {
			return nil, err
		}
		if _, ok := matches[schema]; !ok {
			matches[schema] = make([]bool, len(schema.contexts))
			for id, fields := range schema.contexts {
				matches[schema][id] = slices.Contains(fields, field)
			}
		}
		if slices.Contains(matches[schema], true) {
			kept = append(kept, candidate)
		}
	}
	return r.read(ctx, kept, from, to, func(block *v2Opened) ([]int, error) {
		index := block.slot(v2SlotContext, 0)
		contexts, err := r.decoder.intSlot(block, index)
		if err != nil {
			return nil, err
		}
		wanted := matches[block.schema]
		return v2RowsOf(block, index, func(i int) bool { return wanted[contexts[i]] }), nil
	})
}

// readAttr decodes one attribute column per block and materializes only matching rows
func (r *v2Reader) readAttr(ctx context.Context, field recordField, from, to int64) ([]recordEvent, error) {
	candidates, err := r.candidates(ctx, from, to, 0)
	if err != nil {
		return nil, err
	}
	return r.read(ctx, candidates, from, to, func(block *v2Opened) ([]int, error) {
		column := -1
		for _, shape := range block.schema.shapes {
			if position := slices.Index(shape.keys, field.key); position >= 0 {
				column = shape.columns[position]
				break
			}
		}
		index := block.slot(v2SlotAttr, column)
		if index < 0 {
			return nil, nil
		}
		values, err := r.decoder.valueSlot(block, index)
		if err != nil {
			return nil, err
		}
		return v2RowsOf(block, index, func(i int) bool { return values[i] == field.value }), nil
	})
}

func (r *v2Reader) String() string {
	return fmt.Sprintf("segments=%d blocks=%d decoded=%d bytes=%d", r.segments, r.blocks, r.decoded, r.bytes)
}

func v2StoreSize(ctx context.Context, db *sql.DB) (int64, string, error) {
	if _, err := db.ExecContext(ctx, `vacuum`); err != nil {
		return 0, "", err
	}
	var size int64
	if err := db.QueryRowContext(ctx, `select page_count*page_size from pragma_page_count,pragma_page_size`).Scan(&size); err != nil {
		return 0, "", err
	}
	objects, err := recordFileObjects(ctx, db)
	return size, objects, err
}

func v2OpenStore(ctx context.Context, path string) (*sql.DB, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, err
	}
	db.SetMaxOpenConns(1)
	// a block row of ~8 KB leaves most of a 4 KB leaf page empty; 1 KB pages waste about 1%
	page := "1024"
	if value := os.Getenv("TINYSTORE_RECORD_PAGE"); value != "" {
		page = value
	}
	if _, err = db.ExecContext(ctx, "pragma page_size="+page); err != nil {
		db.Close()
		return nil, err
	}
	if _, err = db.ExecContext(ctx, v2StoreSchema); err != nil {
		db.Close()
		return nil, err
	}
	return db, nil
}
