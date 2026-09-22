//nolint:gosec // checked counts bound allocations; signed timestamp and IEEE conversions preserve bits
package metrics

import (
	"context"
	"database/sql"
	"encoding/binary"
	"fmt"
	"hash/crc32"
	"math"

	"github.com/tinyshed/tinystore/internal/sqlite"

	"github.com/tinyshed/tinystore/codec"
)

const maximumHeadBytes = 16 << 20

type headSnapshot struct {
	seriesID   int64
	count      int
	start, end int64
	from, to   int64
	filtered   bool
	packed     []byte
	legacy     []Sample
	chunks     []headChunk
}

type headChunk struct {
	header codec.Head
	body   []byte
}

func headChecksum(id int64, data []byte) uint32 {
	key := binary.LittleEndian.AppendUint64(nil, uint64(id))
	return crc32.Update(crc32.ChecksumIEEE(key), crc32.IEEETable, data)
}

// each chunk stays within the existing codec's decode bound; the whole tail has its own budget
func (s *Store) encodeHead(ctx context.Context, id int64, points []Sample) ([]byte, error) {
	return s.encodeHeadPrefix(ctx, id, points, nil, 0)
}

func (s *Store) encodeHeadPrefix(ctx context.Context, id int64, points []Sample, prefix []byte, prefixCount int) ([]byte, error) {
	if len(points) == 0 {
		return nil, nil
	}
	if len(points) > s.opts.MaxHeadSamples || prefixCount < 0 || prefixCount > len(points) || prefixCount%blockSamples != 0 {
		return nil, fmt.Errorf("%w: mutable samples in one series", ErrLimit)
	}
	out := binary.AppendUvarint([]byte{1}, uint64(len(points)))
	out = append(out, prefix...)
	if len(out) > s.opts.MaxHeadBytes-4 {
		return nil, fmt.Errorf("%w: mutable head bytes", ErrLimit)
	}
	for start := prefixCount; start < len(points); start += blockSamples {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		head, body, err := s.encoder.Encode(points[start:min(start+blockSamples, len(points))])
		if err != nil {
			return nil, fmt.Errorf("encode mutable head: %w", err)
		}
		out = binary.AppendVarint(out, head.Start)
		out = binary.AppendUvarint(out, uint64(head.End)-uint64(head.Start))
		out = binary.AppendUvarint(out, uint64(head.Count))
		out = binary.LittleEndian.AppendUint64(out, math.Float64bits(head.First))
		out = binary.AppendUvarint(out, uint64(len(body)))
		out = append(out, body...)
		if len(out) > s.opts.MaxHeadBytes-4 {
			return nil, fmt.Errorf("%w: mutable head bytes", ErrLimit)
		}
	}
	return binary.LittleEndian.AppendUint32(out, headChecksum(id, out)), nil
}

func reusableHeadPrefix(packed []byte, before int64, maximumSamples int) ([]byte, int, error) {
	if len(packed) == 0 {
		return nil, 0, nil
	}
	if len(packed) < 6 {
		return nil, 0, fmt.Errorf("%w: mutable head size", ErrCorrupt)
	}
	content := packed[:len(packed)-4]
	r := binaryReader{data: content}
	if r.byte() != 1 {
		return nil, 0, fmt.Errorf("%w: mutable head version", ErrCorrupt)
	}
	count := r.size(maximumSamples)
	if r.err != nil || count == 0 {
		return nil, 0, fmt.Errorf("%w: mutable head count", ErrCorrupt)
	}
	begin := len(content) - len(r.data)
	end := begin
	reused := 0
	for reused < count {
		start := unfoldSigned(r.unsigned())
		span := r.unsigned()
		chunkCount := r.size(min(blockSamples, count-reused))
		r.word()
		length := r.size(maxPayloadBytes)
		r.take(length)
		if r.err != nil || chunkCount == 0 || span > uint64(math.MaxInt64)-uint64(start) {
			return nil, 0, fmt.Errorf("%w: mutable chunk extent", ErrCorrupt)
		}
		if chunkCount < blockSamples || int64(uint64(start)+span) >= before {
			break
		}
		reused += chunkCount
		end = len(content) - len(r.data)
	}
	return content[begin:end], reused, nil
}

func (s *Store) decodeHead(ctx context.Context, head headSnapshot) ([]Sample, error) {
	if head.count < 0 || head.count > s.opts.MaxHeadSamples {
		return nil, fmt.Errorf("%w: mutable head samples", ErrLimit)
	}
	if head.packed == nil {
		if len(head.legacy) != head.count {
			return nil, fmt.Errorf("%w: mutable head sample count", ErrCorrupt)
		}
		return head.legacy, nil
	}
	data := head.packed
	if len(data) < 6 || len(data) > s.opts.MaxHeadBytes {
		return nil, fmt.Errorf("%w: mutable head size", ErrCorrupt)
	}
	content := data[:len(data)-4]
	if headChecksum(head.seriesID, content) != binary.LittleEndian.Uint32(data[len(data)-4:]) {
		return nil, fmt.Errorf("%w: mutable head checksum", ErrCorrupt)
	}
	r := binaryReader{data: content}
	if r.byte() != 1 {
		return nil, fmt.Errorf("%w: mutable head version", ErrCorrupt)
	}
	count := r.size(s.opts.MaxHeadSamples)
	if count != head.count || count == 0 {
		return nil, fmt.Errorf("%w: mutable head count", ErrCorrupt)
	}
	out := make([]Sample, 0, count)
	for len(out) < count {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		start := unfoldSigned(r.unsigned())
		span := r.unsigned()
		n := r.size(min(blockSamples, count-len(out)))
		first := math.Float64frombits(r.word())
		length := r.size(maxPayloadBytes)
		body := r.take(length)
		if r.err != nil {
			return nil, r.err
		}
		if n == 0 || span > uint64(math.MaxInt64)-uint64(start) {
			return nil, fmt.Errorf("%w: mutable chunk extent", ErrCorrupt)
		}
		chunk := codec.Head{Start: start, End: int64(uint64(start) + span), Count: n, First: first}
		if chunk.End == math.MaxInt64 || (len(out) > 0 && start <= out[len(out)-1].At) {
			return nil, fmt.Errorf("%w: mutable chunk ordering", ErrCorrupt)
		}
		iterator, err := s.decoder.Decode(chunk, body)
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
	if err := r.finish(); err != nil {
		return nil, err
	}
	if out[0].At != head.start || out[len(out)-1].At != head.end {
		return nil, fmt.Errorf("%w: mutable head endpoints", ErrCorrupt)
	}
	return out, nil
}

func (s *Store) decodeSelectedHead(ctx context.Context, head headSnapshot) ([]Sample, error) {
	if !head.filtered || head.packed == nil {
		return s.decodeHead(ctx, head)
	}
	chunks := head.chunks
	if chunks == nil {
		var err error
		chunks, err = s.inspectHead(head)
		if err != nil {
			return nil, err
		}
	}
	out := make([]Sample, 0, selectedHeadSamples(chunks, head.from, head.to))
	for _, chunk := range chunks {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		if chunk.header.End < head.from || chunk.header.Start >= head.to {
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
		if chunk.header.End >= from && chunk.header.Start < to {
			count += chunk.header.Count
		}
	}
	return count
}

func (s *Store) inspectHead(head headSnapshot) ([]headChunk, error) {
	data := head.packed
	if len(data) < 6 || len(data) > s.opts.MaxHeadBytes {
		return nil, fmt.Errorf("%w: mutable head size", ErrCorrupt)
	}
	content := data[:len(data)-4]
	if headChecksum(head.seriesID, content) != binary.LittleEndian.Uint32(data[len(data)-4:]) {
		return nil, fmt.Errorf("%w: mutable head checksum", ErrCorrupt)
	}
	r := binaryReader{data: content}
	if r.byte() != 1 {
		return nil, fmt.Errorf("%w: mutable head version", ErrCorrupt)
	}
	count := r.size(s.opts.MaxHeadSamples)
	if count != head.count || count == 0 {
		return nil, fmt.Errorf("%w: mutable head count", ErrCorrupt)
	}
	chunks := make([]headChunk, 0, (count+blockSamples-1)/blockSamples)
	parsed := 0
	var firstStart, previousEnd int64
	for parsed < count {
		start := unfoldSigned(r.unsigned())
		span := r.unsigned()
		n := r.size(min(blockSamples, count-parsed))
		first := math.Float64frombits(r.word())
		length := r.size(maxPayloadBytes)
		body := r.take(length)
		if r.err != nil || n == 0 || length < 8 || (n == 1 && span != 0) || (n > 1 && span == 0) || span > uint64(math.MaxInt64)-uint64(start) {
			return nil, fmt.Errorf("%w: mutable chunk extent", ErrCorrupt)
		}
		chunk := codec.Head{Start: start, End: int64(uint64(start) + span), Count: n, First: first}
		if chunk.End == math.MaxInt64 || (parsed > 0 && start <= previousEnd) {
			return nil, fmt.Errorf("%w: mutable chunk ordering", ErrCorrupt)
		}
		if parsed == 0 {
			firstStart = start
		}
		previousEnd = chunk.End
		parsed += n
		chunks = append(chunks, headChunk{header: chunk, body: body})
	}
	if err := r.finish(); err != nil {
		return nil, err
	}
	if firstStart != head.start || previousEnd != head.end {
		return nil, fmt.Errorf("%w: mutable head endpoints", ErrCorrupt)
	}
	return chunks, nil
}

// the snapshot owns encoded bytes; normal queries decode them only after releasing SQLite
func (s *Store) fetchHead(ctx context.Context, tx sqlite.Reader, id, from, to int64, budget *queryBudget) (headSnapshot, error) {
	head := headSnapshot{seriesID: id}
	var first, last sql.NullInt64
	var size int
	limit := s.opts.MaxHeadBytes
	if budget != nil {
		limit = min(limit, budget.limits.PayloadBytes-budget.bytes)
	}
	err := sqlite.QueryRow(ctx, tx, `select head_count,head_start,head_end,coalesce(length(tail),0),case when length(tail)<=? and head_end>=? and head_start<? then tail else null end from series_state where series_id=?`, limit, from, to, id).Scan(&head.count, &first, &last, &size, &head.packed)
	if err != nil {
		return head, fmt.Errorf("read mutable state: %w", err)
	}
	if head.count == 0 {
		if first.Valid || last.Valid || size != 0 {
			return head, fmt.Errorf("%w: empty mutable head", ErrCorrupt)
		}
		return head, nil
	}
	if !first.Valid || !last.Valid || last.Int64 < first.Int64 {
		return head, fmt.Errorf("%w: mutable endpoints", ErrCorrupt)
	}
	head.start, head.end = first.Int64, last.Int64
	if head.end < from || head.start >= to {
		head.count = 0
		head.packed = nil
		return head, nil
	}
	if head.count < 0 || head.count > s.opts.MaxHeadSamples || size > s.opts.MaxHeadBytes {
		return head, fmt.Errorf("%w: mutable head capacity", ErrLimit)
	}
	if budget != nil {
		bytes := size
		if size == 0 {
			if err = budget.takeSamples(head.count); err != nil {
				return head, err
			}
			bytes = 16 * head.count
		}
		if err = budget.takeBytes(bytes); err != nil {
			return head, err
		}
	}
	if size > 0 {
		if len(head.packed) != size {
			return head, fmt.Errorf("%w: mutable body missing", ErrCorrupt)
		}
		if budget != nil {
			head.filtered, head.from, head.to = true, from, to
			head.chunks, err = s.inspectHead(head)
			if err != nil {
				return head, err
			}
			if err = budget.takeSamples(selectedHeadSamples(head.chunks, from, to)); err != nil {
				return head, err
			}
		}
		return head, nil
	}
	rows, err := tx.QueryContext(ctx, `select at,value from head where series_id=? order by at limit cast(? as integer)`, id, head.count+1)
	if err != nil {
		return head, fmt.Errorf("read legacy head: %w", err)
	}
	defer rows.Close()
	for rows.Next() {
		var point Sample
		var value []byte
		if err = rows.Scan(&point.At, &value); err != nil {
			return head, fmt.Errorf("read legacy sample: %w", err)
		}
		if len(value) != 8 || len(head.legacy) >= head.count {
			return head, fmt.Errorf("%w: legacy head size", ErrCorrupt)
		}
		point.Value = math.Float64frombits(binary.LittleEndian.Uint64(value))
		head.legacy = append(head.legacy, point)
	}
	if err = rows.Err(); err != nil {
		return head, fmt.Errorf("iterate legacy head: %w", err)
	}
	if len(head.legacy) != head.count || head.legacy[0].At != head.start || head.legacy[head.count-1].At != head.end {
		return head, fmt.Errorf("%w: legacy head count or bounds", ErrCorrupt)
	}
	return head, nil
}

func (s *Store) mutablePoints(ctx context.Context, tx sqlite.Reader, id int64) ([]Sample, error) {
	head, err := s.fetchHead(ctx, tx, id, math.MinInt64, math.MaxInt64, nil)
	if err != nil {
		return nil, err
	}
	return s.decodeHead(ctx, head)
}

func (s *Store) saveHead(ctx context.Context, tx *sql.Tx, id int64, points []Sample) error {
	packed, err := s.encodeHead(ctx, id, points)
	if err != nil {
		return err
	}
	var first, last sql.NullInt64
	if len(points) > 0 {
		first = sql.NullInt64{Int64: points[0].At, Valid: true}
		last = sql.NullInt64{Int64: points[len(points)-1].At, Valid: true}
	}
	if _, err = tx.ExecContext(ctx, `update series_state set tail=?,head_count=?,head_start=?,head_end=? where series_id=?`, packed, len(points), first, last, id); err != nil {
		return fmt.Errorf("replace mutable head: %w", err)
	}
	if _, err = tx.ExecContext(ctx, `delete from head where series_id=?`, id); err != nil {
		return fmt.Errorf("retire legacy head: %w", err)
	}
	return nil
}

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

func removeSealed(existing, sealed []Sample) ([]Sample, error) {
	out := make([]Sample, 0, len(existing))
	removed := 0
	for _, point := range existing {
		if removed < len(sealed) && point.At == sealed[removed].At {
			if math.Float64bits(point.Value) != math.Float64bits(sealed[removed].Value) {
				return nil, fmt.Errorf("%w: changed sealed sample", ErrConflict)
			}
			removed++
		} else {
			out = append(out, point)
		}
	}
	if removed != len(sealed) {
		return nil, fmt.Errorf("%w: missing sealed sample", ErrConflict)
	}
	return out, nil
}
