package metrics

import (
	"context"
	"fmt"
)

type queryBudget struct {
	clocks                         map[int64][]storedBlock
	clockOrder                     []int64
	limits                         Limits
	bytes, decoded, blocks, groups int
}

func (b *queryBudget) takeBytes(size int) error {
	if size < 0 || size > b.limits.PayloadBytes-b.bytes {
		return fmt.Errorf("%w: fetched bytes", ErrLimit)
	}
	b.bytes += size
	return nil
}

func (b *queryBudget) takeSamples(count int) error {
	if count < 0 || count > b.limits.DecodedSamples-b.decoded {
		return fmt.Errorf("%w: decoded samples", ErrLimit)
	}
	b.decoded += count
	return nil
}

// Read returns owned samples, or an error with no partial result; the caller holds no SQLite snapshot.
func (s *Store) Read(ctx context.Context, request Range) ([]Result, error) {
	results := []Result{}
	err := s.readEach(ctx, request, func(result Result) error {
		results = append(results, result)
		return nil
	})
	if err != nil {
		return nil, err
	}
	return results, nil
}

// Stream passes one owned series at a time after the snapshot closes; an error may follow earlier results.
func (s *Store) Stream(ctx context.Context, request Range, yield func(Result) error) error {
	if yield == nil {
		return fmt.Errorf("%w: nil stream callback", ErrInvalid)
	}
	return s.readEach(ctx, request, yield)
}

func (s *Store) readEach(ctx context.Context, request Range, yield func(Result) error) error {
	if err := s.enter(ctx); err != nil {
		return err
	}
	defer s.leave()
	select {
	case s.readSlots <- struct{}{}:
	case <-ctx.Done():
		return ctx.Err()
	}
	defer func() { <-s.readSlots }()
	if err := ctx.Err(); err != nil {
		return err
	}
	if request.To < request.From {
		return fmt.Errorf("%w: inverted time range", ErrInvalid)
	}
	matchers, err := orderedLabels(request.Matchers, false)
	if err != nil {
		return err
	}
	limits, err := narrowLimits(request.Limits, s.opts.Limits)
	if err != nil {
		return err
	}
	from := max(request.From, s.cutoff())
	if from >= request.To {
		return nil
	}
	if s.opts.SharedBudget != nil {
		weight, reserveErr := readReservation(limits)
		if reserveErr != nil {
			return reserveErr
		}
		if reserveErr := s.opts.SharedBudget.acquire(ctx, weight); reserveErr != nil {
			return reserveErr
		}
		defer s.opts.SharedBudget.release(weight)
	}
	reads, err := s.fetchSnapshot(ctx, matchers, from, request.To, limits)
	if err != nil {
		return err
	}
	if err := s.visitResults(ctx, reads, from, request.To, limits.OutputSamples, yield); err != nil {
		return err
	}
	s.queried.Add(1)
	return nil
}

func (s *Store) visitResults(ctx context.Context, reads []seriesRead, from, to int64, outputLimit int, yield func(Result) error) error {
	outputCount := 0
	for _, read := range reads {
		result := Result{Series: Series{Labels: read.series.labels, Kind: read.series.kind}}
		appendPoint := func(point Sample) error {
			if point.At < from || point.At >= to {
				return nil
			}
			if outputCount == outputLimit {
				return fmt.Errorf("%w: output samples", ErrLimit)
			}
			if len(result.Samples) > 0 && point.At <= result.Samples[len(result.Samples)-1].At {
				return fmt.Errorf("%w: overlapping samples", ErrCorrupt)
			}
			result.Samples = append(result.Samples, point)
			outputCount++
			return nil
		}
		for _, block := range read.blocks {
			if err := ctx.Err(); err != nil {
				return err
			}
			points, err := s.decodeBlock(block)
			if err != nil {
				return fmt.Errorf("%w: decode values: %w", ErrCorrupt, err)
			}
			for _, point := range points {
				if err = appendPoint(point); err != nil {
					return err
				}
			}
		}
		head, headErr := s.decodeSelectedHead(ctx, read.head)
		if headErr != nil {
			return headErr
		}
		for i, point := range head {
			if i%blockSamples == 0 {
				if err := ctx.Err(); err != nil {
					return err
				}
			}
			if err := appendPoint(point); err != nil {
				return err
			}
		}
		if len(result.Samples) > 0 {
			if err := yield(result); err != nil {
				return fmt.Errorf("stream metrics result: %w", err)
			}
		}
	}
	return nil
}
