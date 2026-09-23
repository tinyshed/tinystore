package metrics

import (
	"context"
	"fmt"
)

func (s *Store) appendPackedHead(ctx context.Context, state ingestState, incoming []Sample, cutoff, id int64) ([]byte, int, int64, int64, int, error) {
	prefix, prefixCount, prefixErr := reusableHeadPrefix(state.head.packed, incoming[0].At, s.opts.MaxHeadSamples)
	if prefixErr != nil {
		return nil, 0, 0, 0, 0, prefixErr
	}
	chunks, inspectErr := s.inspectHead(state.head)
	if inspectErr != nil {
		return nil, 0, 0, 0, 0, inspectErr
	}
	var suffixExisting []Sample
	for _, chunk := range chunks[prefixCount/blockSamples:] {
		if err := ctx.Err(); err != nil {
			return nil, 0, 0, 0, 0, err
		}
		iterator, decodeErr := s.decoder.Decode(chunk.header, chunk.body)
		if decodeErr != nil {
			return nil, 0, 0, 0, 0, fmt.Errorf("%w: mutable chunk: %w", ErrCorrupt, decodeErr)
		}
		for iterator.Next() {
			suffixExisting = append(suffixExisting, iterator.Sample())
		}
		if decodeErr := iterator.Err(); decodeErr != nil {
			return nil, 0, 0, 0, 0, fmt.Errorf("%w: mutable values: %w", ErrCorrupt, decodeErr)
		}
	}
	suffix, mergeErr := mergeHead(suffixExisting, incoming, s.opts.MaxHeadSamples-prefixCount)
	if mergeErr != nil {
		return nil, 0, 0, 0, 0, mergeErr
	}
	packed, encodeErr := s.encodeHeadSuffix(ctx, id, suffix, prefix, prefixCount)
	if encodeErr != nil {
		return nil, 0, 0, 0, 0, encodeErr
	}
	maxSeen := max(state.maxSeen, incoming[len(incoming)-1].At)
	eligible := prefixCount
	for _, point := range suffix {
		if point.At >= cutoff && point.At < maxSeen {
			eligible++
		}
	}
	ready := 0
	if eligible >= blockSamples {
		ready = 1
	}
	return packed, prefixCount + len(suffix), state.head.start, suffix[len(suffix)-1].At, ready, nil
}
