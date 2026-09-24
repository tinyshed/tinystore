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

// encodeTimes writes nothing at all for an evenly spaced block, because the
// head's first and last timestamp and its count already say what the step was
func encodeTimes(head Head, samples []Sample) (byte, []byte) {
	if len(samples) == 1 {
		return timeFixed, nil
	}
	step, divides := head.step()
	fixed := divides
	if fixed {
		for i := 1; i < len(samples); i++ {
			if distance(samples[i-1].At, samples[i].At) != step {
				fixed = false
				break
			}
		}
		if fixed {
			return timeFixed, nil
		}
	}
	var deltas, second []byte
	previous := distance(samples[0].At, samples[1].At)
	second = binary.AppendUvarint(second, previous)
	secondOK := previous <= math.MaxInt64
	for i := 1; i < len(samples); i++ {
		delta := distance(samples[i-1].At, samples[i].At)
		deltas = binary.AppendUvarint(deltas, delta)
		secondOK = secondOK && delta <= math.MaxInt64
		if i > 1 && secondOK {
			second = binary.AppendVarint(second, int64(delta)-int64(previous)) //nolint:gosec // both below MaxInt64
		}
		previous = delta
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
			it.delta = uint64(int64(it.delta) + change) //nolint:gosec // bounded by the checks above
		}
	}
	if it.err != nil {
		return it.err
	}
	if it.delta == 0 || it.delta > distance(it.at, math.MaxInt64) {
		return fmt.Errorf("%w: non-increasing or overflowing timestamp", ErrInvalid)
	}
	it.at = advance(it.at, it.delta)
	return nil
}
