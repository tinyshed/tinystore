package metrics

import (
	"bytes"
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/binary"
	"errors"
	"fmt"
	"hash/crc32"
	"math"

	"github.com/tinyshed/tinystore/codec"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

const maxClockBytes = 65536

func commonDivisor(a, b uint64) uint64 {
	for b != 0 {
		a, b = b, a%b
	}
	return a
}

// a regular block needs no clock body; an irregular one stores units and exceptions
func encodeClockValues(points []Sample) []byte {
	if len(points) < 2 {
		return nil
	}
	deltas := make([]uint64, len(points)-1)
	frequencies := map[uint64]int{}
	unit := uint64(0)
	for i := range deltas {
		deltas[i] = distance(points[i].At, points[i+1].At)
		unit = commonDivisor(unit, deltas[i])
		frequencies[deltas[i]]++
	}
	if len(frequencies) == 1 {
		return nil
	}
	step := deltas[0]
	for delta, count := range frequencies {
		if count > frequencies[step] || (count == frequencies[step] && delta < step) {
			step = delta
		}
	}
	return smallestClock(deltas, unit, step)
}

// smallestClock writes the deltas three ways, in units, and keeps the shortest:
//
//	plain       0, unit, every delta
//	runs        1, unit, then run length and delta, run by run
//	exceptions  2, unit, the commonest step, then the gap to and the delta of each other one
func smallestClock(deltas []uint64, unit, step uint64) []byte {
	plain := binary.AppendUvarint([]byte{0}, unit)
	for _, delta := range deltas {
		plain = binary.AppendUvarint(plain, delta/unit)
	}
	runs := binary.AppendUvarint([]byte{1}, unit)
	for i := 0; i < len(deltas); {
		end := i + 1
		for end < len(deltas) && deltas[end] == deltas[i] {
			end++
		}
		runs = appendCount(runs, end-i)
		runs = binary.AppendUvarint(runs, deltas[i]/unit)
		i = end
	}
	exceptions := binary.AppendUvarint([]byte{2}, unit)
	exceptions = binary.AppendUvarint(exceptions, step/unit)
	previous := -1
	for i, delta := range deltas {
		if delta != step {
			exceptions = appendCount(exceptions, i-previous)
			exceptions = binary.AppendUvarint(exceptions, delta/unit)
			previous = i
		}
	}
	best := plain
	if len(runs) < len(best) {
		best = runs
	}
	if len(exceptions) < len(best) {
		best = exceptions
	}
	return best
}

func decodeClockValues(block storedBlock) ([]int64, error) {
	head := block.head
	if head.Count < 1 || head.Count > blockSamples || head.End < head.Start {
		return nil, fmt.Errorf("%w: clock head", ErrCorrupt)
	}
	if head.Count == 1 {
		if len(block.clock) != 0 || head.Start != head.End {
			return nil, fmt.Errorf("%w: singleton clock", ErrCorrupt)
		}
		return []int64{head.Start}, nil
	}

	deltas, err := clockDeltas(block)
	if err != nil {
		return nil, err
	}

	return accumulateClock(head, deltas)
}

// clockDeltas are the steps between a block's timestamps: all equal for a
// block without a clock body, else as its body encodes them.
func clockDeltas(block storedBlock) ([]uint64, error) {
	head := block.head
	deltas := make([]uint64, head.Count-1)
	if len(block.clock) == 0 {
		span, divisor := distance(head.Start, head.End), uint64(head.Count-1) //nolint:gosec // two or more samples
		if span == 0 || span%divisor != 0 {
			return nil, fmt.Errorf("%w: regular clock step", ErrCorrupt)
		}
		for i := range deltas {
			deltas[i] = span / divisor
		}
		return deltas, nil
	}

	r := binaryReader{data: block.clock}
	mode, unit := r.byte(), r.unsigned()
	switch mode {
	case 0:
		for i := range deltas {
			deltas[i] = r.unsigned()
		}
	case 1:
		readClockRuns(&r, deltas)
	case 2:
		readClockExceptions(&r, deltas)
	default:
		return nil, fmt.Errorf("%w: clock mode", ErrCorrupt)
	}
	if err := r.finish(); err != nil {
		return nil, err
	}
	return deltas, scaleClock(deltas, unit)
}

func readClockRuns(r *binaryReader, deltas []uint64) {
	for i := 0; i < len(deltas) && r.err == nil; {
		count, delta := r.size(len(deltas)-i), r.unsigned()
		if count == 0 {
			r.err = fmt.Errorf("%w: empty clock run", ErrCorrupt)
			return
		}
		for j := range count {
			deltas[i+j] = delta
		}
		i += count
	}
}

func readClockExceptions(r *binaryReader, deltas []uint64) {
	step := r.unsigned()
	for i := range deltas {
		deltas[i] = step
	}
	position := -1
	for len(r.data) > 0 && r.err == nil {
		gap := r.size(len(deltas) - 1 - position)
		if gap == 0 {
			r.err = fmt.Errorf("%w: clock exception position", ErrCorrupt)
			return
		}
		position += gap
		deltas[position] = r.unsigned()
	}
}

// scaleClock turns deltas counted in units into milliseconds.
func scaleClock(deltas []uint64, unit uint64) error {
	if unit == 0 {
		return fmt.Errorf("%w: clock quantum", ErrCorrupt)
	}
	for i, delta := range deltas {
		if delta > math.MaxUint64/unit {
			return fmt.Errorf("%w: clock multiplication", ErrCorrupt)
		}
		deltas[i] *= unit
	}
	return nil
}

// accumulateClock adds the deltas up from the block's start, and refuses a
// clock that does not end exactly at the block's end.
func accumulateClock(head codec.Head, deltas []uint64) ([]int64, error) {
	times := make([]int64, head.Count)
	times[0] = head.Start
	for i, delta := range deltas {
		if delta == 0 || delta > distance(times[i], math.MaxInt64) {
			return nil, fmt.Errorf("%w: clock ordering or overflow", ErrCorrupt)
		}
		times[i+1] = advance(times[i], delta)
	}
	if times[len(times)-1] != head.End {
		return nil, fmt.Errorf("%w: clock endpoint", ErrCorrupt)
	}
	return times, nil
}

func encodeClockGroup(group blockGroup) []byte {
	out := []byte{1, byte(len(group.blocks))} //nolint:gosec // a group holds at most 32 blocks
	for _, block := range group.blocks {
		out = binary.AppendVarint(out, block.head.Start)
		out = binary.AppendUvarint(out, distance(block.head.Start, block.head.End))
		out = appendCount(out, block.head.Count)
		out = appendCount(out, len(block.clock))
		out = append(out, block.clock...)
	}
	return binary.LittleEndian.AppendUint32(out, crc32.ChecksumIEEE(out))
}

func decodeClockGroup(body []byte) ([]storedBlock, error) {
	if len(body) < 6 || len(body) > maxClockBytes {
		return nil, fmt.Errorf("%w: clock object size", ErrCorrupt)
	}
	content := body[:len(body)-4]
	if crc32.ChecksumIEEE(content) != binary.LittleEndian.Uint32(body[len(body)-4:]) {
		return nil, fmt.Errorf("%w: clock object checksum", ErrCorrupt)
	}
	r := binaryReader{data: content}
	if r.byte() != 1 {
		return nil, fmt.Errorf("%w: clock version", ErrCorrupt)
	}
	count := int(r.byte())
	if count < 1 || count > groupSlots {
		return nil, fmt.Errorf("%w: clock slots", ErrCorrupt)
	}
	blocks := make([]storedBlock, count)
	for i := range blocks {
		block, err := readClockBlock(&r)
		if err != nil {
			return nil, err
		}
		if block.head.End == math.MaxInt64 || (i > 0 && block.head.Start <= blocks[i-1].head.End) {
			return nil, fmt.Errorf("%w: clock block ordering", ErrCorrupt)
		}
		if err = checkClockBody(block); err != nil {
			return nil, err
		}
		blocks[i] = block
	}
	if err := r.finish(); err != nil {
		return nil, err
	}
	return blocks, nil
}

func readClockBlock(r *binaryReader) (storedBlock, error) {
	start := unfoldSigned(r.unsigned())
	span := r.unsigned()
	samples := r.size(blockSamples)
	length := r.size(10*blockSamples + 32)
	clock := r.take(length)
	if r.err != nil {
		return storedBlock{}, r.err
	}
	if span > distance(start, math.MaxInt64) || samples < 1 {
		return storedBlock{}, fmt.Errorf("%w: clock extent", ErrCorrupt)
	}
	block := storedBlock{clock: clock}
	block.head.Start = start
	block.head.End = advance(start, span)
	block.head.Count = samples
	return block, nil
}

// checkClockBody refuses a block whose clock body, or the lack of one, does not
// lead from its start to its end in its count of samples.
func checkClockBody(block storedBlock) error {
	samples, span := block.head.Count, distance(block.head.Start, block.head.End)
	if len(block.clock) == 0 {
		if (samples == 1 && span != 0) || (samples > 1 && (span == 0 || span%uint64(samples-1) != 0)) {
			return fmt.Errorf("%w: regular clock extent", ErrCorrupt)
		}
		return nil
	}
	_, err := decodeClockValues(block)
	return err
}

func acquireClock(ctx context.Context, tx *sql.Tx, body []byte) (int64, error) {
	digest := sha256.Sum256(body)
	var id int64
	var saved []byte
	err := tx.QueryRowContext(ctx, `select id,body from clocks where digest=?`, digest[:]).Scan(&id, &saved)
	if err == nil {
		if !bytes.Equal(saved, body) {
			return 0, fmt.Errorf("%w: clock digest collision", ErrCorrupt)
		}
		if _, err = tx.ExecContext(ctx, `update clocks set refs=refs+1 where id=?`, id); err != nil {
			return 0, fmt.Errorf("retain clock: %w", err)
		}
		return id, nil
	}
	if !errors.Is(err, sql.ErrNoRows) {
		return 0, fmt.Errorf("find shared clock: %w", err)
	}
	result, err := tx.ExecContext(ctx, `insert into clocks(digest,refs,body) values(?,1,?)`, digest[:], body)
	if err != nil {
		return 0, fmt.Errorf("create clock: %w", err)
	}
	id, err = result.LastInsertId()
	if err != nil {
		return 0, fmt.Errorf("read clock identifier: %w", err)
	}
	return id, nil
}

func releaseClock(ctx context.Context, tx *sql.Tx, id int64) error {
	result, err := tx.ExecContext(ctx, `update clocks set refs=refs-1 where id=? and refs>0`, id)
	if err != nil {
		return fmt.Errorf("release clock: %w", err)
	}
	n, err := result.RowsAffected()
	if err != nil {
		return fmt.Errorf("count released clocks: %w", err)
	}
	if n != 1 {
		return fmt.Errorf("%w: missing clock owner", ErrCorrupt)
	}
	if _, err = tx.ExecContext(ctx, `delete from clocks where id=? and refs=0`, id); err != nil {
		return fmt.Errorf("remove unused clock: %w", err)
	}
	return nil
}

const clockQuery = `select length(body),case when length(body)<=? then body else null end from clocks where id=?`

func loadClock(ctx context.Context, tx sqlite.Reader, id int64, budget *queryBudget) ([]storedBlock, error) {
	if budget != nil {
		if cached, ok := budget.clocks[id]; ok {
			return cached, nil
		}
	}
	limit := maxClockBytes
	if budget != nil {
		limit = min(limit, budget.limits.PayloadBytes-budget.bytes)
	}
	var size int
	var body []byte
	err := sqlite.QueryRowByKey(ctx, tx, clockQuery, limit, id).Scan(&size, &body)
	if errors.Is(err, sql.ErrNoRows) {
		return nil, fmt.Errorf("%w: shared clock missing", ErrCorrupt)
	}
	if err != nil {
		return nil, fmt.Errorf("read shared clock: %w", err)
	}
	if size > maxClockBytes {
		return nil, fmt.Errorf("%w: shared clock size", ErrCorrupt)
	}
	if budget != nil {
		if err = budget.takeBytes(size); err != nil {
			return nil, err
		}
	}
	blocks, err := decodeClockGroup(body)
	if err != nil {
		return nil, err
	}
	if budget != nil {
		if budget.clocks == nil {
			budget.clocks = make(map[int64][]storedBlock)
		}
		if len(budget.clockOrder) == 8 {
			delete(budget.clocks, budget.clockOrder[0])
			budget.clockOrder = budget.clockOrder[1:]
		}
		budget.clocks[id] = blocks
		budget.clockOrder = append(budget.clockOrder, id)
	}
	return blocks, nil
}
