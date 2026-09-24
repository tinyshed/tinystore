package codec

type bitWriter struct {
	data []byte
	used uint8
}

func (w *bitWriter) put(v uint64, n uint8) {
	for n > 0 {
		if w.used == 0 {
			w.data = append(w.data, 0)
		}
		take := min(n, 8-w.used)
		part := byte((v >> (n - take)) & (1<<take - 1)) //nolint:gosec // take is at most eight bits
		w.data[len(w.data)-1] |= part << (8 - w.used - take)
		w.used = (w.used + take) % 8
		n -= take
	}
}

type bitReader struct {
	data     []byte
	position int
	failed   bool
}

func (r *bitReader) get(n uint8) uint64 {
	if r.failed || int(n) > len(r.data)*8-r.position {
		r.failed = true
		return 0
	}
	var value uint64
	for n > 0 {
		used := uint8(r.position % 8) //nolint:gosec // position is a nonnegative bit offset
		take := min(n, 8-used)
		part := r.data[r.position/8] >> (8 - used - take) & byte(1<<take-1)
		value = value<<take | uint64(part)
		r.position += int(take)
		n -= take
	}
	return value
}

func (r *bitReader) finished() bool {
	left := len(r.data)*8 - r.position
	return !r.failed && left >= 0 && left < 8 && r.get(uint8(left)) == 0
}

// distance is end − start as an unsigned number, exact across the whole signed range.
func distance(start, end int64) uint64 {
	return uint64(end) - uint64(start) //nolint:gosec // modular subtraction is the exact distance
}

// advance is start + span; callers have checked the result stays representable.
func advance(start int64, span uint64) int64 {
	return int64(uint64(start) + span) //nolint:gosec // bounded by the caller's distance check
}

// foldSigned is zigzag: small magnitudes of either sign become small numbers.
func foldSigned(value int64) uint64 {
	return uint64(value)<<1 ^ uint64(value>>63) //nolint:gosec // a bit transform, not a value converted
}
