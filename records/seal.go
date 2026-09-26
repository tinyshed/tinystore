package records

import (
	"context"
	"database/sql"
	"errors"
	"fmt"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// headChunk is the head rows one segment is made of: whole rows in arrival
// order, as many as fit a segment's bounds, less the rows known to be damaged
type headChunk struct {
	ids    []int64
	spans  [][2]int64 // each row's first and last time
	bodies [][]byte
	headWeight
	oldest int64 // when its first row was written
	full   bool  // the head's next row would not fit
}

// sealHead seals a head a segment at a time: a full segment at once, and what
// is left once its oldest row has waited SealAge. A row that no longer reads
// is reported once and left where it is; the rest of its head seals.
func (p *maintenancePass) sealHead(ctx context.Context, head headKey) error {
	for {
		chunk, err := p.store.readChunk(ctx, head)
		if err != nil {
			return err
		}
		if len(chunk.ids) == 0 || (!chunk.full && chunk.oldest > p.sealBefore()) {
			return nil
		}
		err = p.store.noted(p.seal(ctx, head, chunk))
		var damage *DamageError
		if errors.As(err, &damage) {
			p.result.Damaged++
			continue
		}
		if err != nil {
			return err
		}
	}
}

// seal encodes one chunk outside the writer, then publishes it
func (p *maintenancePass) seal(ctx context.Context, head headKey, chunk headChunk) error {
	unreserve, err := p.store.reserve(ctx, func() int64 { return segmentReservation })
	if err != nil {
		return err
	}
	defer unreserve()

	records, err := p.store.chunkRecords(head, chunk)
	if err != nil {
		return err
	}

	segment := newEncoder(p.store.blobs).encodeSegment(p.store.streams.name(head.stream), records)
	if err = p.store.publish(ctx, head, chunk, segment); err != nil {
		return err
	}

	p.result.SealedSegments++
	p.result.SealedRecords += segment.count
	p.sealedStreams[head.stream] = true
	p.store.sealed.Add(1)
	return nil
}

const selectHeadRows = `
	select id, first_at, last_at, count, input, written_at, body from heads
	where stream = ? and late = ?
	order by id
	limit cast(? as integer)`

// errChunkFull stops reading a head at the first row a segment cannot take
var errChunkFull = errors.New("the segment is full")

// readChunk reads, in one snapshot, the rows of a head one segment can take;
// a row holds at least one record, so a segment's worth plus one is enough
func (s *Store) readChunk(ctx context.Context, head headKey) (headChunk, error) {
	var chunk headChunk
	err := s.file.ViewPrepared(ctx, func(tx sqlite.Reader) error {
		//nolint:rowserrcheck // EachRow checks Err
		rows, err := tx.QueryContext(ctx, selectHeadRows, head.stream, head.late, maxSegmentRecords+1)
		if err != nil {
			return err
		}
		return sqlite.EachRow(rows, "head rows", func(rows *sql.Rows) error {
			return chunk.add(rows, s.damaged.headRow)
		})
	})
	if errors.Is(err, errChunkFull) {
		chunk.full, err = true, nil
	}
	if err != nil {
		return headChunk{}, fmt.Errorf("records: read head of %s: %w", s.streams.name(head.stream), err)
	}
	chunk.full = chunk.full || chunk.count == maxSegmentRecords || chunk.input == maxSegmentInput
	return chunk, nil
}

func (c *headChunk) add(rows *sql.Rows, damaged func(id int64) bool) error {
	var id, first, last, writtenAt int64
	var count, input int
	var body []byte
	if err := rows.Scan(&id, &first, &last, &count, &input, &writtenAt, &body); err != nil {
		return err
	}
	if damaged(id) {
		return nil
	}
	if c.count+count > maxSegmentRecords || c.input+input > maxSegmentInput {
		return errChunkFull
	}
	if len(c.ids) == 0 {
		c.oldest = writtenAt
	}
	c.ids, c.spans, c.bodies = append(c.ids, id), append(c.spans, [2]int64{first, last}), append(c.bodies, body)
	c.count, c.input = c.count+count, c.input+input
	return nil
}

// chunkRecords decodes a chunk's rows into its records, in arrival order
func (s *Store) chunkRecords(head headKey, chunk headChunk) ([]Record, error) {
	d := newDecoder(s.unpack)
	stream := s.streams.name(head.stream)
	records := make([]Record, 0, chunk.count)
	for i, body := range chunk.bodies {
		rows, err := d.parseHeadRow(stream, body)
		if err != nil {
			first, last := chunk.spans[i][0], chunk.spans[i][1]
			found := Damage{Stream: stream, HeadRow: chunk.ids[i], From: timeOf(first), To: timeOf(last)}
			return nil, damageOf(found, err)
		}
		records = append(records, rows...)
	}
	return records, nil
}
