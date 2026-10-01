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

// Read returns owned samples, or an error with no partial result; the caller
// holds no SQLite snapshot.
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

// Stream passes one owned series at a time after the snapshot closes; an error
// may follow earlier results.
func (s *Store) Stream(ctx context.Context, request Range, yield func(Result) error) error {
	if yield == nil {
		return fmt.Errorf("%w: nil stream callback", ErrInvalid)
	}
	return s.readEach(ctx, request, yield)
}

func (s *Store) readEach(ctx context.Context, request Range, yield func(Result) error) error {
	release, err := s.admit(ctx, s.readSlots)
	if err != nil {
		return err
	}
	defer release()

	query, err := s.checkRange(request)
	if err != nil || query.from >= query.to {
		return err
	}

	unreserve, err := s.reserve(ctx, func() (int64, error) { return readReservation(query.limits) })
	if err != nil {
		return err
	}
	defer unreserve()

	reads, err := s.fetchSnapshot(ctx, query)
	if err != nil {
		return err
	}

	if err = s.yieldResults(ctx, query, reads, yield); err != nil {
		return err
	}
	s.queried.Add(1)
	return nil
}

// rangeQuery is a checked request: exact matchers, the limits it may spend and
// its range, whose start retention may have moved forward.
type rangeQuery struct {
	matchers  []Label
	limits    Limits
	from, to  int64
	aggregate *aggregateSelection
}

func (s *Store) checkRange(request Range) (rangeQuery, error) {
	if request.To < request.From {
		return rangeQuery{}, fmt.Errorf("%w: inverted time range", ErrInvalid)
	}
	matchers, err := orderedLabels(request.Matchers, false)
	if err != nil {
		return rangeQuery{}, err
	}
	limits, err := narrowLimits(request.Limits, s.opts.Limits)
	if err != nil {
		return rangeQuery{}, err
	}
	return rangeQuery{matchers: matchers, limits: limits, from: max(request.From, s.cutoff()), to: request.To}, nil
}

// yieldResults hands each nonempty series to yield, with its samples inside
// the range and within the output limit across every series.
func (s *Store) yieldResults(
	ctx context.Context, query rangeQuery, reads []seriesRead, yield func(Result) error,
) error {
	output := 0
	for _, read := range reads {
		result := Result{Series: Series{Labels: read.series.labels, Kind: read.series.kind}}
		err := s.eachSample(ctx, read, func(point Sample) error {
			if point.At < query.from || point.At >= query.to {
				return nil
			}
			if output == query.limits.OutputSamples {
				return fmt.Errorf("%w: output samples", ErrLimit)
			}
			if len(result.Samples) > 0 && point.At <= result.Samples[len(result.Samples)-1].At {
				return fmt.Errorf("%w: overlapping samples", ErrCorrupt)
			}
			result.Samples = append(result.Samples, point)
			output++
			return nil
		})
		if err != nil {
			return err
		}
		if len(result.Samples) > 0 {
			if err = yield(result); err != nil {
				return fmt.Errorf("stream metrics result: %w", err)
			}
		}
	}
	return nil
}

// eachSample decodes a series' blocks, then its head, and hands every sample
// to visit in time order; it checks for cancellation once per block.
func (s *Store) eachSample(ctx context.Context, read seriesRead, visit func(Sample) error) error {
	for _, block := range read.blocks {
		if err := s.visitBlock(ctx, block, visit); err != nil {
			return err
		}
	}
	return s.visitHead(ctx, read.head, visit)
}

func (s *Store) visitBlock(ctx context.Context, block storedBlock, visit func(Sample) error) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	points, err := s.decodeBlock(block)
	if err != nil {
		return fmt.Errorf("%w: decode values: %w", ErrCorrupt, err)
	}
	for _, point := range points {
		if err = visit(point); err != nil {
			return err
		}
	}
	return nil
}

func (s *Store) visitHead(ctx context.Context, selected headSnapshot, visit func(Sample) error) error {
	head, err := s.decodeSelectedHead(ctx, selected)
	if err != nil {
		return err
	}
	for i, point := range head {
		if i%blockSamples == 0 {
			if err = ctx.Err(); err != nil {
				return err
			}
		}
		if err = visit(point); err != nil {
			return err
		}
	}
	return nil
}
