package metrics

import (
	"context"
	"encoding/binary"
	"fmt"
	"hash/crc32"
	"math"

	"github.com/tinyshed/tinystore/codec"
)

// The head is the mutable tail of one series, stored whole in series_state.tail:
//
//	┌────┬───────┬─────────┬─────────┬───┬─────────┬───────┐
//	│ v1 │ count │ chunk 1 │ chunk 2 │ … │ chunk n │ crc32 │
//	└────┴───────┴─────────┴─────────┴───┴─────────┴───────┘
//
//	chunk = start │ end − start │ samples │ first value │ body length │ codec body
//
// A chunk holds at most 240 samples, so each one decodes inside the codec's
// bound. The checksum also covers the series id: a head copied onto another
// series does not verify.
const (
	headVersion      = 1
	maximumHeadBytes = 16 << 20
)

// headSnapshot is a head as its row holds it, before anything is decoded.
type headSnapshot struct {
	seriesID   int64
	count      int
	start, end int64
	packed     []byte

	// a query that needs only part of the head parses it once in the snapshot
	filtered bool
	from, to int64
	chunks   []headChunk
}

// headChunk is one chunk of a parsed head.
type headChunk struct {
	header codec.Head
	body   []byte
	stored []byte // the whole chunk as stored, copied as is when a write keeps it
}

func (c headChunk) overlaps(from, to int64) bool {
	return c.header.End >= from && c.header.Start < to
}

// parseHead checks a head against its row and splits it into chunks. It is the
// only reader of the layout above; decoding, reuse and partial reads start here.
func (s *Store) parseHead(head headSnapshot) ([]headChunk, error) {
	data := head.packed
	if len(data) < 6 || len(data) > s.opts.MaxHeadBytes {
		return nil, fmt.Errorf("%w: mutable head size", ErrCorrupt)
	}
	content, sum := data[:len(data)-4], binary.LittleEndian.Uint32(data[len(data)-4:])
	if headChecksum(head.seriesID, content) != sum {
		return nil, fmt.Errorf("%w: mutable head checksum", ErrCorrupt)
	}

	r := binaryReader{data: content}
	if r.byte() != headVersion {
		return nil, fmt.Errorf("%w: mutable head version", ErrCorrupt)
	}
	count := r.size(s.opts.MaxHeadSamples)
	if count != head.count || count == 0 {
		return nil, fmt.Errorf("%w: mutable head count", ErrCorrupt)
	}

	chunks := make([]headChunk, 0, (count+blockSamples-1)/blockSamples)
	for parsed := 0; parsed < count; {
		chunk, err := readHeadChunk(&r, count-parsed)
		if err != nil {
			return nil, err
		}
		if len(chunks) > 0 && chunk.header.Start <= chunks[len(chunks)-1].header.End {
			return nil, fmt.Errorf("%w: mutable chunk ordering", ErrCorrupt)
		}
		chunks = append(chunks, chunk)
		parsed += chunk.header.Count
	}
	if err := r.finish(); err != nil {
		return nil, err
	}

	if chunks[0].header.Start != head.start || chunks[len(chunks)-1].header.End != head.end {
		return nil, fmt.Errorf("%w: mutable head endpoints", ErrCorrupt)
	}
	return chunks, nil
}

// readHeadChunk reads one chunk; remaining bounds its sample count.
func readHeadChunk(r *binaryReader, remaining int) (headChunk, error) {
	stored := r.data
	start := unfoldSigned(r.unsigned())
	span := r.unsigned()
	count := r.size(min(blockSamples, remaining))
	first := math.Float64frombits(r.word())
	length := r.size(maxPayloadBytes)
	body := r.take(length)
	if r.err != nil {
		return headChunk{}, r.err
	}

	switch {
	case count == 0, length < 8, count == 1 && span != 0, count > 1 && span == 0:
		return headChunk{}, fmt.Errorf("%w: mutable chunk extent", ErrCorrupt)
	case span > distance(start, math.MaxInt64):
		return headChunk{}, fmt.Errorf("%w: mutable chunk extent", ErrCorrupt)
	}
	end := advance(start, span)
	if end == math.MaxInt64 {
		return headChunk{}, fmt.Errorf("%w: mutable chunk ordering", ErrCorrupt)
	}

	return headChunk{
		header: codec.Head{Start: start, End: end, Count: count, First: first},
		body:   body,
		stored: stored[:len(stored)-len(r.data)],
	}, nil
}

// decodeHead returns every sample of a head.
func (s *Store) decodeHead(ctx context.Context, head headSnapshot) ([]Sample, error) {
	if head.count < 0 || head.count > s.opts.MaxHeadSamples {
		return nil, fmt.Errorf("%w: mutable head samples", ErrLimit)
	}
	if head.packed == nil {
		if head.count != 0 {
			return nil, fmt.Errorf("%w: mutable head sample count", ErrCorrupt)
		}
		return nil, nil
	}

	chunks, err := s.parseHead(head)
	if err != nil {
		return nil, err
	}
	return s.decodeChunks(ctx, chunks, math.MinInt64, math.MaxInt64)
}

// decodeSelectedHead decodes only the chunks a query range touches:
//
//	chunks    [10:00 … 10:59] [11:00 … 11:59] [12:00 … 12:20]
//	range              10:30 ────── 11:10
//	decoded   [10:00 … 10:59] [11:00 … 11:59]        the last is skipped
func (s *Store) decodeSelectedHead(ctx context.Context, head headSnapshot) ([]Sample, error) {
	if !head.filtered || head.packed == nil {
		return s.decodeHead(ctx, head)
	}

	chunks := head.chunks
	if chunks == nil {
		var err error
		if chunks, err = s.parseHead(head); err != nil {
			return nil, err
		}
	}
	return s.decodeChunks(ctx, chunks, head.from, head.to)
}

func (s *Store) decodeChunks(ctx context.Context, chunks []headChunk, from, to int64) ([]Sample, error) {
	out := make([]Sample, 0, selectedHeadSamples(chunks, from, to))
	for _, chunk := range chunks {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		if !chunk.overlaps(from, to) {
			continue
		}

		iterator, err := s.decoder.Decode(chunk.header, chunk.body)
		if err != nil {
			return nil, fmt.Errorf("%w: mutable chunk: %w", ErrCorrupt, err)
		}
		for iterator.Next() {
			out = append(out, iterator.Sample())
		}
		if err = iterator.Err(); err != nil {
			return nil, fmt.Errorf("%w: mutable values: %w", ErrCorrupt, err)
		}
	}
	return out, nil
}

func selectedHeadSamples(chunks []headChunk, from, to int64) int {
	count := 0
	for _, chunk := range chunks {
		if chunk.overlaps(from, to) {
			count += chunk.header.Count
		}
	}
	return count
}

// encodeHead packs samples into a new head.
func (s *Store) encodeHead(ctx context.Context, id int64, points []Sample) ([]byte, error) {
	return s.encodeHeadAfter(ctx, id, nil, points)
}

// encodeHeadAfter packs points behind chunks that are kept byte for byte:
//
//	kept      [240][240]
//	points              [240][ 57]         encoded now
//	result    v1 │ 777 │ [240][240][240][ 57] │ crc32
func (s *Store) encodeHeadAfter(ctx context.Context, id int64, kept []headChunk, points []Sample) ([]byte, error) {
	total := countChunkSamples(kept) + len(points)
	if total == 0 {
		return nil, nil
	}
	if total > s.opts.MaxHeadSamples {
		return nil, fmt.Errorf("%w: mutable samples in one series", ErrLimit)
	}

	out := appendCount([]byte{headVersion}, total)
	for _, chunk := range kept {
		out = append(out, chunk.stored...)
	}
	if len(out) > s.opts.MaxHeadBytes-4 {
		return nil, fmt.Errorf("%w: mutable head bytes", ErrLimit)
	}

	for start := 0; start < len(points); start += blockSamples {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		var err error
		out, err = s.appendHeadChunk(out, points[start:min(start+blockSamples, len(points))])
		if err != nil {
			return nil, err
		}
		if len(out) > s.opts.MaxHeadBytes-4 {
			return nil, fmt.Errorf("%w: mutable head bytes", ErrLimit)
		}
	}

	return binary.LittleEndian.AppendUint32(out, headChecksum(id, out)), nil
}

// appendHeadChunk encodes up to 240 samples as one chunk.
func (s *Store) appendHeadChunk(out []byte, points []Sample) ([]byte, error) {
	header, body, err := s.encoder.Encode(points)
	if err != nil {
		return nil, fmt.Errorf("encode mutable head: %w", err)
	}
	out = binary.AppendVarint(out, header.Start)
	out = binary.AppendUvarint(out, distance(header.Start, header.End))
	out = appendCount(out, header.Count)
	out = binary.LittleEndian.AppendUint64(out, math.Float64bits(header.First))
	out = appendCount(out, len(body))
	return append(out, body...), nil
}

// reusableChunks returns the leading chunks a write at `before` leaves as they
// are: full, and ending before it.
//
//	chunks      [0 … 239] [240 … 479] [480 … 520]
//	write at 300               ↑
//	reusable    [0 … 239]                            copied byte for byte
func reusableChunks(chunks []headChunk, before int64) []headChunk {
	reusable := 0
	for _, chunk := range chunks {
		if chunk.header.Count < blockSamples || chunk.header.End >= before {
			break
		}
		reusable++
	}
	return chunks[:reusable]
}

func countChunkSamples(chunks []headChunk) int {
	count := 0
	for _, chunk := range chunks {
		count += chunk.header.Count
	}
	return count
}

// mergeHead merges two sorted runs; a repeated timestamp takes the incoming value:
//
//	existing   10  11       13
//	incoming       11'  12       14
//	merged     10  11'  12  13   14
func mergeHead(existing, incoming []Sample, limit int) ([]Sample, error) {
	merged := make([]Sample, 0, min(len(existing)+len(incoming), limit))
	a, b := 0, 0
	for a < len(existing) || b < len(incoming) {
		var point Sample
		switch {
		case b == len(incoming) || (a < len(existing) && existing[a].At < incoming[b].At):
			point = existing[a]
			a++
		case a == len(existing) || incoming[b].At < existing[a].At:
			point = incoming[b]
			b++
		default:
			point = incoming[b]
			a++
			b++
		}
		if len(merged) == limit {
			return nil, fmt.Errorf("%w: mutable samples in one series", ErrLimit)
		}
		merged = append(merged, point)
	}
	return merged, nil
}

// removeSealed drops exactly the samples a new block holds. A sample that changed
// since maintenance read it means a write won the race, so publication stops.
func removeSealed(existing, sealed []Sample) ([]Sample, error) {
	out := make([]Sample, 0, len(existing))
	removed := 0
	for _, point := range existing {
		if removed < len(sealed) && point.At == sealed[removed].At {
			if math.Float64bits(point.Value) != math.Float64bits(sealed[removed].Value) {
				return nil, fmt.Errorf("%w: changed sealed sample", ErrConflict)
			}
			removed++
			continue
		}
		out = append(out, point)
	}
	if removed != len(sealed) {
		return nil, fmt.Errorf("%w: missing sealed sample", ErrConflict)
	}
	return out, nil
}

func headChecksum(id int64, data []byte) uint32 {
	key := binary.LittleEndian.AppendUint64(nil, uint64(id)) //nolint:gosec // identifier bits, not a magnitude
	return crc32.Update(crc32.ChecksumIEEE(key), crc32.IEEETable, data)
}

// distance is end − start as an unsigned number, exact across the whole signed range.
func distance(start, end int64) uint64 {
	return uint64(end) - uint64(start) //nolint:gosec // modular subtraction is the exact distance
}

// advance is start + span; callers have checked the result stays representable.
func advance(start int64, span uint64) int64 {
	return int64(uint64(start) + span) //nolint:gosec // bounded by the caller's distance check
}

func appendCount(out []byte, count int) []byte {
	return binary.AppendUvarint(out, uint64(count)) //nolint:gosec // counts are never negative
}
