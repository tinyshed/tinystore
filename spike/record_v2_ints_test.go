package spike

import (
	"encoding/binary"
	"math"
	"math/bits"
	"slices"

	"github.com/klauspost/compress/fse"
)

// an integer column is a transform, a base with a divisor, and a packer:
//
//	sorted times   1000 1250 1250 1900
//	delta               250    0  650
//	base 0, gcd 50        5    0   13   → rice k=2
const (
	v2Plain byte = iota
	v2Delta
	v2Trend
)

const (
	v2Bitpack byte = iota
	v2Radix
	v2Rice
	v2Entropy
)

const (
	v2Direct byte = iota
	v2Dictionary
	v2Sparse
)

const (
	v2RiceEscape      = 32
	v2DictionaryLimit = 256
)

type v2IntPlan struct {
	transform   byte
	first, step int64
	base        int64
	divisor     uint64
	packer      byte
	width       uint
	group       uint
	radix       uint64
	rice        uint
	entropy     []byte
	bytes       int
}

// v2Bits writes least significant bit first, 64 bits at a time
type v2Bits struct {
	out  []byte
	acc  uint64
	used uint
}

func (w *v2Bits) write(value uint64, width uint) {
	for width > 0 {
		take := min(width, 64-w.used)
		part := value
		if take < 64 {
			part &= 1<<take - 1
		}
		w.acc |= part << w.used
		w.used += take
		value >>= take
		width -= take
		if w.used == 64 {
			w.out = binary.LittleEndian.AppendUint64(w.out, w.acc)
			w.acc, w.used = 0, 0
		}
	}
}

func (w *v2Bits) finish() []byte {
	for w.used > 0 {
		w.out = append(w.out, byte(w.acc))
		w.acc >>= 8
		w.used -= min(w.used, 8)
	}
	return w.out
}

type v2BitReader struct {
	data []byte
	at   uint64
	bad  bool
}

func (r *v2BitReader) read(width uint) uint64 {
	var value uint64
	for got := uint(0); got < width; {
		index := r.at / 8
		if index >= uint64(len(r.data)) {
			r.bad = true
			return 0
		}
		shift := uint(r.at % 8)
		take := min(8-shift, width-got)
		value |= (uint64(r.data[index]>>shift) & (1<<take - 1)) << got
		got += take
		r.at += uint64(take)
	}
	return value
}

func v2SourceLen(n int, transform byte) int {
	if transform == v2Delta {
		return max(0, n-1)
	}
	return n
}

// transforms wrap around on overflow, and so does their inverse
func v2Source(values []int64, plan *v2IntPlan, i int) int64 {
	switch plan.transform {
	case v2Delta:
		return values[i+1] - values[i]
	case v2Trend:
		return values[i] - (plan.first + plan.step*int64(i))
	}
	return values[i]
}

func (p *v2IntPlan) residual(values []int64, i int) uint64 {
	return (uint64(v2Source(values, p, i)) - uint64(p.base)) / p.divisor
}

func v2UvarintLen(value uint64) int {
	return (bits.Len64(value|1) + 6) / 7
}

func v2VarintLen(value int64) int {
	return v2UvarintLen(uint64(value<<1) ^ uint64(value>>63))
}

func v2IntHeader(plan *v2IntPlan) int {
	size := 3 + v2VarintLen(plan.base) + v2UvarintLen(plan.divisor)
	switch plan.transform {
	case v2Delta:
		size += v2VarintLen(plan.first)
	case v2Trend:
		size += v2VarintLen(plan.first) + v2VarintLen(plan.step)
	}
	switch plan.packer {
	case v2Bitpack, v2Rice:
		size++
	case v2Radix:
		size += 1 + v2UvarintLen(plan.radix)
	}
	return size
}

func (e *v2Encoder) planInts(values []int64) v2IntPlan {
	best := e.planTransform(values, v2IntPlan{transform: v2Plain})
	if len(values) >= 2 {
		delta := e.planTransform(values, v2IntPlan{transform: v2Delta, first: values[0]})
		if delta.bytes < best.bytes {
			best = delta
		}
	}
	if len(values) >= 16 {
		if step, ok := v2TrendStep(values); ok {
			trend := e.planTransform(values, v2IntPlan{transform: v2Trend, first: values[0], step: step})
			if trend.bytes < best.bytes {
				best = trend
			}
		}
	}
	return best
}

func v2TrendStep(values []int64) (int64, bool) {
	n := float64(len(values))
	meanX, meanY := (n-1)/2, 0.0
	for _, value := range values {
		meanY += float64(value-values[0]) / n
	}
	covariance, variance := 0.0, 0.0
	for i, value := range values {
		x := float64(i) - meanX
		covariance += x * (float64(value-values[0]) - meanY)
		variance += x * x
	}
	step := math.Round(covariance / variance)
	if math.IsNaN(step) || step == 0 || step < -0x1p62 || step > 0x1p62 {
		return 0, false
	}
	return int64(step), true
}

func (e *v2Encoder) planTransform(values []int64, plan v2IntPlan) v2IntPlan {
	m := v2SourceLen(len(values), plan.transform)
	plan.divisor = 1
	if m == 0 {
		plan.bytes = v2IntHeader(&plan)
		return plan
	}
	minimum, maximum := v2Source(values, &plan, 0), v2Source(values, &plan, 0)
	for i := 1; i < m; i++ {
		source := v2Source(values, &plan, i)
		minimum, maximum = min(minimum, source), max(maximum, source)
	}
	plan.base = minimum
	divisor := uint64(0)
	for i := 0; i < m && divisor != 1; i++ {
		divisor = recordGCD(divisor, uint64(v2Source(values, &plan, i))-uint64(minimum))
	}
	plan.divisor = max(divisor, 1)
	return e.choosePacker(values, plan, m, (uint64(maximum)-uint64(minimum))/plan.divisor)
}

func (e *v2Encoder) choosePacker(values []int64, plan v2IntPlan, m int, top uint64) v2IntPlan {
	plan.packer, plan.width = v2Bitpack, uint(bits.Len64(top))
	best := uint64(m) * uint64(plan.width)
	if top < math.MaxUint64 {
		if group, width, cost := v2RadixCost(top+1, m); cost < best {
			plan.packer, plan.group, plan.width, plan.radix, best = v2Radix, group, width, top+1, cost
		}
	}
	if k, cost := v2RiceCost(values, &plan, m); cost < best {
		plan.packer, plan.rice, best = v2Rice, k, cost
	}
	plan.bytes = v2IntHeader(&plan) + int((best+7)/8)
	if plan.packer == v2Rice {
		plan.bytes += v2UvarintLen((best + 7) / 8)
	}
	if top < 256 && m >= 32 {
		plan = e.tryEntropy(values, plan, m)
	}
	return plan
}

func v2RadixCost(radix uint64, m int) (uint, uint, uint64) {
	if radix < 3 || radix&(radix-1) == 0 {
		return 0, 0, math.MaxUint64
	}
	best, group, width, product := uint64(math.MaxUint64), uint(0), uint(0), uint64(1)
	for size := uint(1); ; size++ {
		high, low := bits.Mul64(product, radix)
		if high != 0 {
			return group, width, best
		}
		product = low
		wordWidth := uint(bits.Len64(product - 1))
		words := (uint64(m) + uint64(size) - 1) / uint64(size)
		if cost := words * uint64(wordWidth); cost < best {
			best, group, width = cost, size, wordWidth
		}
	}
}

func v2RiceBits(value uint64, k uint) uint64 {
	if quotient := value >> k; quotient < v2RiceEscape {
		return quotient + 1 + uint64(k)
	}
	return v2RiceEscape + 64
}

// the best k is within a bit of log2 of the mean, so only four are measured
func v2RiceCost(values []int64, plan *v2IntPlan, m int) (uint, uint64) {
	sum := 0.0
	for i := range m {
		sum += float64(plan.residual(values, i))
	}
	center := 0
	if mean := sum / float64(m); mean >= 1 {
		center = int(math.Log2(mean))
	}
	var costs [4]uint64
	for i := range m {
		value := plan.residual(values, i)
		for j := range costs {
			costs[j] += v2RiceBits(value, uint(min(63, max(0, center-1+j))))
		}
	}
	best := 0
	for j := range costs {
		if costs[j] < costs[best] {
			best = j
		}
	}
	return uint(min(63, max(0, center-1+best))), costs[best]
}

// fse is tried only when the histogram says it can beat the fixed-width packers
func (e *v2Encoder) tryEntropy(values []int64, plan v2IntPlan, m int) v2IntPlan {
	var counts [256]int
	e.symbols = e.symbols[:0]
	for i := range m {
		symbol := byte(plan.residual(values, i))
		counts[symbol]++
		e.symbols = append(e.symbols, symbol)
	}
	entropy, used := 0.0, 0
	for _, count := range counts {
		if count > 0 {
			p := float64(count) / float64(m)
			entropy -= float64(count) * math.Log2(p)
			used++
		}
	}
	estimate := int(entropy/8) + used + 8
	if float64(estimate) > 0.9*float64(plan.bytes) {
		return plan
	}
	out, err := fse.Compress(e.symbols, &e.fse)
	if err != nil {
		return plan
	}
	candidate := plan
	candidate.packer, candidate.entropy = v2Entropy, slices.Clone(out)
	candidate.bytes = v2IntHeader(&candidate) + v2UvarintLen(uint64(len(out))) + len(out)
	if candidate.bytes < plan.bytes {
		return candidate
	}
	return plan
}

func (e *v2Encoder) appendPlan(out []byte, values []int64, plan *v2IntPlan) []byte {
	out = append(out, v2Direct, plan.transform)
	switch plan.transform {
	case v2Delta:
		out = binary.AppendVarint(out, plan.first)
	case v2Trend:
		out = binary.AppendVarint(binary.AppendVarint(out, plan.first), plan.step)
	}
	out = binary.AppendUvarint(binary.AppendVarint(out, plan.base), plan.divisor)
	out = append(out, plan.packer)
	m := v2SourceLen(len(values), plan.transform)
	switch plan.packer {
	case v2Bitpack:
		writer := v2Bits{out: append(out, byte(plan.width))}
		for i := range m {
			writer.write(plan.residual(values, i), plan.width)
		}
		return writer.finish()
	case v2Radix:
		return e.appendRadix(append(binary.AppendUvarint(out, plan.radix), byte(plan.group)), values, plan, m)
	case v2Rice:
		writer := v2Bits{out: e.rice[:0]}
		for i := range m {
			v2WriteRice(&writer, plan.residual(values, i), plan.rice)
		}
		e.rice = writer.finish()
		out = binary.AppendUvarint(append(out, byte(plan.rice)), uint64(len(e.rice)))
		return append(out, e.rice...)
	}
	return append(binary.AppendUvarint(out, uint64(len(plan.entropy))), plan.entropy...)
}

func (e *v2Encoder) appendRadix(out []byte, values []int64, plan *v2IntPlan, m int) []byte {
	writer := v2Bits{out: out}
	for start := 0; start < m; start += int(plan.group) {
		word, scale := uint64(0), uint64(1)
		for i := start; i < min(m, start+int(plan.group)); i++ {
			word += plan.residual(values, i) * scale
			scale *= plan.radix
		}
		writer.write(word, plan.width)
	}
	return writer.finish()
}

func v2WriteRice(writer *v2Bits, value uint64, k uint) {
	if quotient := value >> k; quotient < v2RiceEscape {
		writer.write(1<<quotient-1, uint(quotient))
		writer.write(0, 1)
		writer.write(value, k)
		return
	}
	writer.write(1<<v2RiceEscape-1, v2RiceEscape)
	writer.write(value, 64)
}

func v2ReadRice(reader *v2BitReader, k uint) uint64 {
	quotient := uint64(0)
	for quotient < v2RiceEscape && reader.read(1) == 1 {
		quotient++
	}
	if quotient == v2RiceEscape {
		return reader.read(64)
	}
	return quotient<<k | reader.read(k)
}

// appendInts writes the smallest of the direct plan, a dictionary and a sparse layout
func (e *v2Encoder) appendInts(out []byte, values []int64) []byte {
	plan := e.planInts(values)
	start := len(out)
	out = e.appendPlan(out, values, &plan)
	if len(values) < 8 {
		return out
	}
	e.countInts(values)
	if alternative := e.dictionaryInts(values, len(out)-start); alternative != nil {
		out = append(out[:start], alternative...)
	}
	if alternative := e.sparseInts(values, len(out)-start); alternative != nil {
		out = append(out[:start], alternative...)
	}
	return out
}

func (e *v2Encoder) appendNestedInts(out []byte, values []int64) []byte {
	plan := e.planInts(values)
	return e.appendPlan(out, values, &plan)
}

func (e *v2Encoder) countInts(values []int64) {
	clear(e.counts)
	for _, value := range values {
		e.counts[value]++
		if len(e.counts) > v2DictionaryLimit {
			return
		}
	}
}

func (e *v2Encoder) dictionaryInts(values []int64, budget int) []byte {
	if len(e.counts) > v2DictionaryLimit || len(e.counts)*4 > len(values) {
		return nil
	}
	words := make([]int64, 0, len(e.counts))
	for value := range e.counts {
		words = append(words, value)
	}
	slices.Sort(words)
	ids := make([]int64, len(values))
	for i, value := range values {
		index, _ := slices.BinarySearch(words, value)
		ids[i] = int64(index)
	}
	out := binary.AppendUvarint([]byte{v2Dictionary}, uint64(len(words)))
	out = e.appendNestedInts(e.appendNestedInts(out, words), ids)
	if len(out) >= budget {
		return nil
	}
	return out
}

func (e *v2Encoder) sparseInts(values []int64, budget int) []byte {
	if len(e.counts) > v2DictionaryLimit {
		return nil
	}
	mode, frequency := int64(0), 0
	for value, count := range e.counts {
		if count > frequency || (count == frequency && value < mode) {
			mode, frequency = value, count
		}
	}
	if frequency*8 < len(values)*7 || frequency == len(values) {
		return nil
	}
	var positions, exceptions []int64
	for i, value := range values {
		if value != mode {
			positions, exceptions = append(positions, int64(i)), append(exceptions, value)
		}
	}
	out := binary.AppendUvarint(binary.AppendVarint([]byte{v2Sparse}, mode), uint64(len(positions)))
	out = e.appendNestedInts(e.appendNestedInts(out, positions), exceptions)
	if len(out) >= budget {
		return nil
	}
	return out
}

func (d *v2Decoder) ints(cursor *recordCursor, count int, nested bool) []int64 {
	switch method := byte(cursor.number(2)); {
	case method == v2Dictionary && !nested:
		return d.dictionaryInts(cursor, count)
	case method == v2Sparse && !nested:
		return d.sparseInts(cursor, count)
	case method == v2Direct:
		return d.directInts(cursor, count)
	}
	cursor.fail("integer column method")
	return nil
}

func (d *v2Decoder) dictionaryInts(cursor *recordCursor, count int) []int64 {
	words := d.ints(cursor, cursor.number(min(count, v2DictionaryLimit)), true)
	ids := d.ints(cursor, count, true)
	if cursor.err != nil {
		return nil
	}
	values := make([]int64, count)
	for i, id := range ids {
		if id < 0 || id >= int64(len(words)) {
			cursor.fail("integer dictionary reference")
			return nil
		}
		values[i] = words[id]
	}
	return values
}

func (d *v2Decoder) sparseInts(cursor *recordCursor, count int) []int64 {
	mode := cursor.signed()
	exceptions := cursor.number(count)
	positions := d.ints(cursor, exceptions, true)
	replacements := d.ints(cursor, exceptions, true)
	if cursor.err != nil {
		return nil
	}
	values := make([]int64, count)
	for i := range values {
		values[i] = mode
	}
	for i, position := range positions {
		if position < 0 || position >= int64(count) || (i > 0 && positions[i-1] >= position) {
			cursor.fail("sparse integer position")
			return nil
		}
		values[position] = replacements[i]
	}
	return values
}

func (d *v2Decoder) directInts(cursor *recordCursor, count int) []int64 {
	plan := v2IntPlan{transform: byte(cursor.number(2))}
	switch plan.transform {
	case v2Delta:
		plan.first = cursor.signed()
	case v2Trend:
		plan.first, plan.step = cursor.signed(), cursor.signed()
	}
	plan.base, plan.divisor, plan.packer = cursor.signed(), cursor.unsigned(), byte(cursor.number(3))
	if plan.divisor == 0 || (plan.transform == v2Delta && count == 0) {
		cursor.fail("integer divisor or delta count")
		return nil
	}
	residuals := d.residuals(cursor, &plan, v2SourceLen(count, plan.transform))
	if cursor.err != nil {
		return nil
	}
	return v2Restore(&plan, residuals, count)
}

func v2Restore(plan *v2IntPlan, residuals []uint64, count int) []int64 {
	values := make([]int64, count)
	for i, residual := range residuals {
		source := int64(uint64(plan.base) + residual*plan.divisor)
		switch plan.transform {
		case v2Delta:
			if i == 0 {
				values[0] = plan.first
			}
			values[i+1] = values[i] + source
		case v2Trend:
			values[i] = plan.first + plan.step*int64(i) + source
		default:
			values[i] = source
		}
	}
	if plan.transform == v2Delta && len(residuals) == 0 && count == 1 {
		values[0] = plan.first
	}
	return values
}

func (d *v2Decoder) residuals(cursor *recordCursor, plan *v2IntPlan, m int) []uint64 {
	switch plan.packer {
	case v2Bitpack:
		width := uint(cursor.number(64))
		reader := v2BitReader{data: cursor.take((m*int(width) + 7) / 8)}
		return v2ReadWidth(cursor, &reader, m, width)
	case v2Radix:
		return v2ReadRadix(cursor, m)
	case v2Rice:
		k := uint(cursor.number(63))
		reader := v2BitReader{data: cursor.take(cursor.number(recordWorkLimit))}
		residuals := make([]uint64, m)
		for i := range residuals {
			residuals[i] = v2ReadRice(&reader, k)
		}
		if reader.bad || (reader.at+7)/8 != uint64(len(reader.data)) {
			cursor.fail("rice stream length")
		}
		return residuals
	}
	return d.entropyResiduals(cursor, m)
}

func v2ReadWidth(cursor *recordCursor, reader *v2BitReader, m int, width uint) []uint64 {
	if cursor.err != nil {
		return nil
	}
	residuals := make([]uint64, m)
	for i := range residuals {
		residuals[i] = reader.read(width)
	}
	if reader.bad {
		cursor.fail("packed integers truncated")
	}
	return residuals
}

func v2ReadRadix(cursor *recordCursor, m int) []uint64 {
	radix, group := cursor.unsigned(), uint(cursor.number(64))
	product, ok := uint64(1), radix >= 3 && group >= 1
	for range group {
		high, low := bits.Mul64(product, radix)
		ok = ok && high == 0
		product = low
	}
	if !ok || cursor.err != nil {
		cursor.fail("radix bounds")
		return nil
	}
	width := uint(bits.Len64(product - 1))
	words := (m + int(group) - 1) / int(group)
	reader := v2BitReader{data: cursor.take((words*int(width) + 7) / 8)}
	packed := v2ReadWidth(cursor, &reader, words, width)
	residuals := make([]uint64, m)
	for i, word := range packed {
		for j := i * int(group); j < (i+1)*int(group); j++ {
			if j < m {
				residuals[j] = word % radix
			}
			word /= radix
		}
		if word != 0 {
			cursor.fail("radix word")
		}
	}
	return residuals
}

func (d *v2Decoder) entropyResiduals(cursor *recordCursor, m int) []uint64 {
	data := cursor.take(cursor.number(recordWorkLimit))
	if cursor.err != nil || m == 0 {
		cursor.fail("entropy coded integers")
		return nil
	}
	// a zero limit would let fse expand up to 2 GB
	d.fse.DecompressLimit = m
	symbols, err := fse.Decompress(data, &d.fse)
	if err != nil || len(symbols) != m {
		cursor.fail("entropy coded integers")
		return nil
	}
	residuals := make([]uint64, m)
	for i, symbol := range symbols {
		residuals[i] = uint64(symbol)
	}
	return residuals
}
