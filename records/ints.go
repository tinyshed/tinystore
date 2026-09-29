package records

import (
	"encoding/binary"
	"math"
	"math/bits"
	"slices"

	"github.com/klauspost/compress/fse"
)

// an integer column is written the cheapest exact way, chosen by computed size:
// directly, as a dictionary, or as one dominant value with exceptions. Written
// directly, it is a transform, a base with a common divisor, and a packer:
//
//	sorted times   1000 1250 1250 1900
//	delta               250    0  650
//	base 0, gcd 50        5    0   13   → 4 bits a value
const (
	layoutDirect byte = iota
	layoutDictionary
	layoutDominant
)

const (
	transformNone byte = iota
	transformDelta
	transformTrend
)

const (
	packWidth byte = iota
	packRadix
	packRice
	packFSE
)

// intPlan is how one column is written directly, and what that costs in bytes
type intPlan struct {
	transform   byte
	first, step int64
	base        int64
	divisor     uint64
	packer      byte
	width       uint   // bits a value, or a radix word
	group       uint   // values a radix word holds
	radix       uint64 // one more than the largest residual
	rice        uint   // the rice parameter k
	fse         []byte // the entropy-coded residuals
	bytes       int
}

// appendInts writes the smallest of the direct plan, a dictionary and a
// dominant value with exceptions
func (e *encoder) appendInts(out []byte, values []int64) []byte {
	start := len(out)
	out = e.appendDirectInts(out, values)
	if len(values) < 8 {
		return out
	}
	e.countInts(values)
	if smaller := e.dictionaryInts(values, len(out)-start); smaller != nil {
		out = append(out[:start], smaller...)
	}
	if smaller := e.dominantInts(values, len(out)-start); smaller != nil {
		out = append(out[:start], smaller...)
	}
	return out
}

// appendDirectInts is the one layout a column nested inside another may use
func (e *encoder) appendDirectInts(out []byte, values []int64) []byte {
	plan := e.planInts(values)
	return e.writePlan(out, values, &plan)
}

func (e *encoder) planInts(values []int64) intPlan {
	best := e.planTransform(values, intPlan{transform: transformNone})
	if len(values) >= 2 {
		delta := e.planTransform(values, intPlan{transform: transformDelta, first: values[0]})
		if delta.bytes < best.bytes {
			best = delta
		}
	}
	if len(values) >= 16 {
		if step, ok := trendStep(values); ok {
			trend := e.planTransform(values, intPlan{transform: transformTrend, first: values[0], step: step})
			if trend.bytes < best.bytes {
				best = trend
			}
		}
	}
	return best
}

// trendStep is the least-squares slope, rounded; each product is converted on
// its own, so that no platform fuses it and picks a different step
func trendStep(values []int64) (int64, bool) {
	n := float64(len(values))
	meanX, meanY := (n-1)/2, 0.0
	for _, value := range values {
		meanY += float64(value-values[0]) / n
	}
	covariance, variance := 0.0, 0.0
	for i, value := range values {
		x := float64(i) - meanX
		covariance += float64(x * (float64(value-values[0]) - meanY))
		variance += float64(x * x)
	}
	step := math.Round(covariance / variance)
	if math.IsNaN(step) || step == 0 || step < -0x1p62 || step > 0x1p62 {
		return 0, false
	}
	return int64(step), true
}

// sourceLen is how many residuals a transform leaves: a delta keeps its first value aside
func sourceLen(n int, transform byte) int {
	if transform == transformDelta {
		return max(0, n-1)
	}
	return n
}

// source is the value a residual is taken from; every transform wraps around on
// overflow, and so does its inverse in restore
func (p *intPlan) source(values []int64, i int) int64 {
	switch p.transform {
	case transformDelta:
		return values[i+1] - values[i]
	case transformTrend:
		return values[i] - (p.first + p.step*int64(i))
	}
	return values[i]
}

func (p *intPlan) residual(values []int64, i int) uint64 {
	return distance(p.base, p.source(values, i)) / p.divisor
}

func (e *encoder) planTransform(values []int64, plan intPlan) intPlan {
	m := sourceLen(len(values), plan.transform)
	plan.divisor = 1
	if m == 0 {
		plan.bytes = planHeader(&plan)
		return plan
	}
	lowest, highest := plan.source(values, 0), plan.source(values, 0)
	for i := 1; i < m; i++ {
		source := plan.source(values, i)
		lowest, highest = min(lowest, source), max(highest, source)
	}
	plan.base = lowest
	divisor := uint64(0)
	for i := 0; i < m && divisor != 1; i++ {
		divisor = gcd(divisor, distance(lowest, plan.source(values, i)))
	}
	plan.divisor = max(divisor, 1)
	return e.choosePacker(values, plan, m, distance(lowest, highest)/plan.divisor)
}

func gcd(a, b uint64) uint64 {
	for b != 0 {
		a, b = b, a%b
	}
	return a
}

// choosePacker prices a fixed width, radix words and a rice code, and tries fse
// on a small alphabet
func (e *encoder) choosePacker(values []int64, plan intPlan, m int, top uint64) intPlan {
	plan.packer, plan.width = packWidth, uint(bits.Len64(top))
	best := unsigned(m) * uint64(plan.width)
	if top < math.MaxUint64 {
		if group, width, cost := radixCost(top+1, m); cost < best {
			plan.packer, plan.group, plan.width, plan.radix, best = packRadix, group, width, top+1, cost
		}
	}
	if k, cost := riceCost(values, &plan, m); cost < best {
		plan.packer, plan.rice, best = packRice, k, cost
	}
	packed := int((best + 7) / 8) //nolint:gosec // the bits of at most a column of 64-bit values
	plan.bytes = planHeader(&plan) + packed
	if plan.packer == packRice {
		plan.bytes += uvarintLen(unsigned(packed))
	}
	if top < 256 && m >= 32 {
		plan = e.tryFSE(values, plan, m)
	}
	return plan
}

// radixCost packs as many values into one word as its width allows: three
// values below 5 fit 7 bits (125 < 128) where fixed width takes 9
func radixCost(radix uint64, m int) (group, width uint, cost uint64) {
	if radix < 3 || radix&(radix-1) == 0 {
		return 0, 0, math.MaxUint64
	}
	cost, product := uint64(math.MaxUint64), uint64(1)
	for size := uint(1); ; size++ {
		high, low := bits.Mul64(product, radix)
		if high != 0 {
			return group, width, cost
		}
		product = low
		wordWidth := uint(bits.Len64(product - 1))
		words := (unsigned(m) + uint64(size) - 1) / uint64(size)
		if total := words * uint64(wordWidth); total < cost {
			cost, group, width = total, size, wordWidth
		}
	}
}

// riceCost is the cheapest rice parameter of all 64 and its bits, exactly and
// in one pass: a value of bit length L costs 1+k bits when k is at least L,
// (v>>k)+1+k while L is at most k+5, and the escape beyond. The mean is a
// poor guide, since a few wide gaps raise it for every value:
//
//	14 zeros and 1000000 twice   k 0: 14×1 + 2×96 = 206 bits
//	                             k 16, near log2 of the mean: 14×17 + 2×(15+17) = 302 bits
func riceCost(values []int64, plan *intPlan, m int) (k uint, cost uint64) {
	var residuals riceHistogram
	for i := range m {
		residuals.add(plan.residual(values, i))
	}
	cost = math.MaxUint64
	// a k past the longest residual only adds bits
	for candidate := range min(residuals.longest, 63) + 1 {
		if total := residuals.cost(candidate); total < cost {
			k, cost = uint(candidate), total
		}
	}
	return k, cost
}

// a quotient below riceEscape, 32, is at most five bits
const riceQuotientBits = 5

// riceHistogram counts residuals by bit length and sums them shifted to one to
// five bits, which prices every k exactly
type riceHistogram struct {
	counts  [65]uint64
	shifted [65][riceQuotientBits + 1]uint64
	longest int
}

func (h *riceHistogram) add(value uint64) {
	length := bits.Len64(value)
	h.counts[length]++
	h.longest = max(h.longest, length)
	for above := 1; above <= riceQuotientBits && above <= length; above++ {
		h.shifted[length][above] += value >> (length - above)
	}
}

func (h *riceHistogram) cost(k int) uint64 {
	total := uint64(0)
	for length := range h.longest + 1 {
		count := h.counts[length]
		switch {
		case count == 0:
		case length <= k:
			total += count * unsigned(1+k)
		case length <= k+riceQuotientBits:
			total += h.shifted[length][length-k] + count*unsigned(1+k)
		default:
			total += count * (riceEscape + 64)
		}
	}
	return total
}

// tryFSE runs the entropy coder only when the histogram says it can win
func (e *encoder) tryFSE(values []int64, plan intPlan, m int) intPlan {
	var counts [256]int
	e.symbols = e.symbols[:0]
	for i := range m {
		symbol := byte(plan.residual(values, i)) //nolint:gosec // tried only when every residual is below 256
		counts[symbol]++
		e.symbols = append(e.symbols, symbol)
	}
	entropy, used := 0.0, 0
	for _, count := range counts {
		if count > 0 {
			entropy -= float64(count) * math.Log2(float64(count)/float64(m))
			used++
		}
	}
	if estimate := int(entropy/8) + used + 8; float64(estimate) > 0.9*float64(plan.bytes) {
		return plan
	}
	packed, err := fse.Compress(e.symbols, &e.fse)
	if err != nil {
		return plan
	}
	candidate := plan
	candidate.packer, candidate.fse = packFSE, slices.Clone(packed)
	candidate.bytes = planHeader(&candidate) + uvarintLen(uint64(len(packed))) + len(packed)
	if candidate.bytes < plan.bytes {
		return candidate
	}
	return plan
}

func uvarintLen(value uint64) int {
	return (bits.Len64(value|1) + 6) / 7
}

func varintLen(value int64) int {
	return uvarintLen(zigzag(value))
}

// planHeader is the size of everything a direct column writes before its residuals
func planHeader(plan *intPlan) int {
	size := 3 + varintLen(plan.base) + uvarintLen(plan.divisor)
	switch plan.transform {
	case transformDelta:
		size += varintLen(plan.first)
	case transformTrend:
		size += varintLen(plan.first) + varintLen(plan.step)
	}
	switch plan.packer {
	case packWidth, packRice:
		size++
	case packRadix:
		size += 1 + uvarintLen(plan.radix)
	}
	return size
}

func (e *encoder) writePlan(out []byte, values []int64, plan *intPlan) []byte {
	out = append(out, layoutDirect, plan.transform)
	switch plan.transform {
	case transformDelta:
		out = binary.AppendVarint(out, plan.first)
	case transformTrend:
		out = binary.AppendVarint(binary.AppendVarint(out, plan.first), plan.step)
	}
	out = binary.AppendUvarint(binary.AppendVarint(out, plan.base), plan.divisor)
	out = append(out, plan.packer)
	m := sourceLen(len(values), plan.transform)
	switch plan.packer {
	case packWidth:
		writer := bitWriter{out: append(out, parameter(plan.width))}
		for i := range m {
			writer.write(plan.residual(values, i), plan.width)
		}
		return writer.finish()
	case packRadix:
		out = append(binary.AppendUvarint(out, plan.radix), parameter(plan.group))
		return e.writeRadix(out, values, plan, m)
	case packRice:
		return e.writeRice(append(out, parameter(plan.rice)), values, plan, m)
	}
	return append(appendCount(out, len(plan.fse)), plan.fse...)
}

func (e *encoder) writeRadix(out []byte, values []int64, plan *intPlan, m int) []byte {
	writer := bitWriter{out: out}
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

// writeRice needs the code's length before the code, so it is built aside first
func (e *encoder) writeRice(out []byte, values []int64, plan *intPlan, m int) []byte {
	writer := bitWriter{out: e.rice[:0]}
	for i := range m {
		writeRice(&writer, plan.residual(values, i), plan.rice)
	}
	e.rice = writer.finish()
	return append(appendCount(out, len(e.rice)), e.rice...)
}

func (e *encoder) countInts(values []int64) {
	clear(e.counts)
	for _, value := range values {
		e.counts[value]++
		if len(e.counts) > maxIntDictionary {
			return
		}
	}
}

// dictionaryInts writes the distinct values once, sorted, and an id per value
func (e *encoder) dictionaryInts(values []int64, budget int) []byte {
	if len(e.counts) > maxIntDictionary || len(e.counts)*4 > len(values) {
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
	out := appendCount([]byte{layoutDictionary}, len(words))
	out = e.appendDirectInts(e.appendDirectInts(out, words), ids)
	if len(out) >= budget {
		return nil
	}
	return out
}

// dominantInts writes the value seven in eight share once, and where the others are
func (e *encoder) dominantInts(values []int64, budget int) []byte {
	if len(e.counts) > maxIntDictionary {
		return nil
	}
	dominant, frequency := int64(0), 0
	for value, count := range e.counts {
		if count > frequency || (count == frequency && value < dominant) {
			dominant, frequency = value, count
		}
	}
	if frequency*8 < len(values)*7 || frequency == len(values) {
		return nil
	}
	var positions, exceptions []int64
	for i, value := range values {
		if value != dominant {
			positions, exceptions = append(positions, int64(i)), append(exceptions, value)
		}
	}
	out := appendCount(binary.AppendVarint([]byte{layoutDominant}, dominant), len(positions))
	out = e.appendDirectInts(e.appendDirectInts(out, positions), exceptions)
	if len(out) >= budget {
		return nil
	}
	return out
}

// ints reads a column of count values in any layout
func (d *decoder) ints(c *cursor, count int) []int64 {
	switch layout := c.readByte(); layout {
	case layoutDictionary:
		return d.dictionaryInts(c, count)
	case layoutDominant:
		return d.dominantInts(c, count)
	case layoutDirect:
		return d.plannedInts(c, count)
	}
	c.fail("integer column layout")
	return nil
}

// directInts reads a column nested inside another, which is always direct
func (d *decoder) directInts(c *cursor, count int) []int64 {
	if c.readByte() != layoutDirect {
		c.fail("nested integer column layout")
		return nil
	}
	return d.plannedInts(c, count)
}

func (d *decoder) dictionaryInts(c *cursor, count int) []int64 {
	words := d.directInts(c, c.count(min(count, maxIntDictionary)))
	ids := d.directInts(c, count)
	if c.err != nil {
		return nil
	}
	values := make([]int64, count)
	for i, id := range ids {
		if id < 0 || id >= int64(len(words)) {
			c.fail("integer dictionary reference")
			return nil
		}
		values[i] = words[id]
	}
	return values
}

func (d *decoder) dominantInts(c *cursor, count int) []int64 {
	dominant := c.varint()
	exceptions := c.count(count)
	positions := d.directInts(c, exceptions)
	replacements := d.directInts(c, exceptions)
	if c.err != nil {
		return nil
	}
	values := make([]int64, count)
	for i := range values {
		values[i] = dominant
	}
	for i, position := range positions {
		if position < 0 || position >= int64(count) || (i > 0 && positions[i-1] >= position) {
			c.fail("integer exception position")
			return nil
		}
		values[position] = replacements[i]
	}
	return values
}

func (d *decoder) plannedInts(c *cursor, count int) []int64 {
	plan := intPlan{transform: c.readByte()}
	switch plan.transform {
	case transformNone:
	case transformDelta:
		plan.first = c.varint()
	case transformTrend:
		plan.first, plan.step = c.varint(), c.varint()
	default:
		c.fail("integer transform")
		return nil
	}
	plan.base, plan.divisor, plan.packer = c.varint(), c.uvarint(), c.readByte()
	if plan.divisor == 0 || (plan.transform == transformDelta && count == 0) {
		c.fail("integer divisor or delta count")
		return nil
	}
	residuals := d.residuals(c, &plan, sourceLen(count, plan.transform))
	if c.err != nil {
		return nil
	}
	return restore(&plan, residuals, count)
}

// restore inverts the transform, wrapping around exactly as the encoder did
func restore(plan *intPlan, residuals []uint64, count int) []int64 {
	values := make([]int64, count)
	if plan.transform == transformDelta {
		values[0] = plan.first
	}
	for i, residual := range residuals {
		source := advance(plan.base, residual*plan.divisor)
		switch plan.transform {
		case transformDelta:
			values[i+1] = values[i] + source
		case transformTrend:
			values[i] = plan.first + plan.step*int64(i) + source
		default:
			values[i] = source
		}
	}
	return values
}

func (d *decoder) residuals(c *cursor, plan *intPlan, m int) []uint64 {
	switch plan.packer {
	case packWidth:
		width := uint(c.count(64))
		return readWidth(c, c.take((m*int(width)+7)/8), m, width)
	case packRadix:
		return readRadix(c, m)
	case packRice:
		return readRiceCode(c, m)
	case packFSE:
		return d.fseResiduals(c, m)
	}
	c.fail("integer packer")
	return nil
}

func readWidth(c *cursor, data []byte, m int, width uint) []uint64 {
	if c.err != nil {
		return nil
	}
	reader := bitReader{data: data}
	residuals := make([]uint64, m)
	for i := range residuals {
		residuals[i] = reader.read(width)
	}
	if reader.short {
		c.fail("packed integers truncated")
	}
	return residuals
}

func readRadix(c *cursor, m int) []uint64 {
	radix, group := c.uvarint(), uint(c.count(64))
	product, fits := uint64(1), radix >= 3 && group >= 1
	for range group {
		high, low := bits.Mul64(product, radix)
		fits = fits && high == 0
		product = low
	}
	if !fits || c.err != nil {
		c.fail("radix bounds")
		return nil
	}
	width := uint(bits.Len64(product - 1))
	words := (m + int(group) - 1) / int(group)
	packed := readWidth(c, c.take((words*int(width)+7)/8), words, width)
	residuals := make([]uint64, m)
	for i, word := range packed {
		for j := i * int(group); j < (i+1)*int(group); j++ {
			if j < m {
				residuals[j] = word % radix
			}
			word /= radix
		}
		if word != 0 {
			c.fail("radix word")
		}
	}
	return residuals
}

func readRiceCode(c *cursor, m int) []uint64 {
	k := uint(c.count(63))
	reader := bitReader{data: c.take(c.count(maxExpandedText))}
	if c.err != nil {
		return nil
	}
	residuals := make([]uint64, m)
	for i := range residuals {
		residuals[i] = readRice(&reader, k)
	}
	if reader.short || reader.consumed() != uint64(len(reader.data)) {
		c.fail("rice code length")
	}
	return residuals
}

func (d *decoder) fseResiduals(c *cursor, m int) []uint64 {
	data := c.take(c.count(maxExpandedText))
	if c.err != nil || m == 0 {
		c.fail("entropy coded integers")
		return nil
	}
	// a zero limit would let fse expand up to 2 GB
	d.fse.DecompressLimit = m
	symbols, err := fse.Decompress(data, &d.fse)
	if err != nil || len(symbols) != m {
		c.fail("entropy coded integers")
		return nil
	}
	residuals := make([]uint64, m)
	for i, symbol := range symbols {
		residuals[i] = uint64(symbol)
	}
	return residuals
}
