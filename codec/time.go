package codec

import (
	"encoding/binary"
	"fmt"
	"math"
)

const (
	timeFixed byte = iota
	timeDelta
	timeDeltaDelta
)

func encodeTimes(samples []Sample) (byte, []byte) {
	first := binary.LittleEndian.AppendUint64(nil, uint64(samples[0].At)) //nolint:gosec // encode the signed timestamp bits
	if len(samples) == 1 {
		return timeFixed, first
	}
	step := uint64(samples[1].At) - uint64(samples[0].At) //nolint:gosec // ordered signed timestamps can span the full uint64 range
	fixed := true
	deltas := append([]byte(nil), first...)
	second := append([]byte(nil), first...)
	second = binary.AppendUvarint(second, step)
	previous := step
	secondOK := step <= math.MaxInt64
	for i := 1; i < len(samples); i++ {
		delta := uint64(samples[i].At) - uint64(samples[i-1].At) //nolint:gosec // modular subtraction preserves a positive full-width distance
		fixed = fixed && delta == step
		deltas = binary.AppendUvarint(deltas, delta)
		secondOK = secondOK && delta <= math.MaxInt64
		if i > 1 && secondOK {
			second = binary.AppendVarint(second, int64(delta)-int64(previous)) //nolint:gosec // secondOK bounds both deltas by MaxInt64
		}
		previous = delta
	}
	if fixed {
		return timeFixed, binary.AppendUvarint(first, step)
	}
	if secondOK && len(second) < len(deltas) {
		return timeDeltaDelta, second
	}
	return timeDelta, deltas
}

func (it *Iterator) nextTime() error {
	switch it.timeMode {
	case timeDelta:
		it.delta, it.err = takeUnsigned(&it.times)
	case timeDeltaDelta:
		if it.index == 1 {
			it.delta, it.err = takeUnsigned(&it.times)
		} else {
			var change int64
			change, it.err = takeSigned(&it.times)
			if it.err == nil && (it.delta > math.MaxInt64 ||
				(change > 0 && int64(it.delta) > math.MaxInt64-change) ||
				(change <= 0 && change <= -int64(it.delta))) {
				return fmt.Errorf("%w: timestamp delta overflow", ErrInvalid)
			}
			it.delta = uint64(int64(it.delta) + change) //nolint:gosec // the preceding checks bound the positive result by MaxInt64
		}
	}
	if it.err != nil {
		return it.err
	}
	if it.delta == 0 || it.delta > uint64(math.MaxInt64)-uint64(it.at) { //nolint:gosec // modular subtraction computes the full positive distance to MaxInt64
		return fmt.Errorf("%w: non-increasing or overflowing timestamp", ErrInvalid)
	}
	it.at = int64(uint64(it.at) + it.delta) //nolint:gosec // the bound above prevents signed timestamp overflow
	return nil
}
