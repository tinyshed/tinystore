package metrics

import (
	"context"
	"fmt"
	"math"
)

type queryBudget struct {
	clocks                         map[int64][]storedBlock
	clockOrder                     []int64
	limits                         Limits
	bytes, decoded, blocks, groups int
	series, summarized             int // what a plan reports
}

func (b *queryBudget) takeBytes(size int) error {
	if size < 0 || size > b.limits.PayloadBytes-b.bytes {
		return limit(LimitPayloadBytes, b.bytes+size, b.limits.PayloadBytes)
	}
	b.bytes += size
	return nil
}

func (b *queryBudget) takeSamples(count int) error {
	if count < 0 || count > b.limits.DecodedSamples-b.decoded {
		return limit(LimitDecodedSamples, b.decoded+count, b.limits.DecodedSamples)
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
// its range, whose start retention may have moved forward from the origin it
// was asked for.
type rangeQuery struct {
	matchers         []label
	conditions       []condition
	planOnly         bool // a Plan: no payload fetched, nothing decoded
	limits           Limits
	origin, from, to int64
	cutoff           int64 // retention's, read once a query
	aggregate        *aggregateSelection
}

func (s *Store) checkRange(request Range) (rangeQuery, error) {
	origin, to, err := s.bounds(request)
	if err != nil {
		return rangeQuery{}, err
	}
	if request.Name == "" && len(request.Match) == 0 && !finds(request.Where) {
		return rangeQuery{}, fmt.Errorf("%w: a range names a series, a label to match, or a OneOf or a Prefix",
			ErrInvalid)
	}
	kept, err := keptLabels(request.Name, request.Match)
	if err != nil {
		return rangeQuery{}, err
	}
	var matchers []label
	if len(kept) > 0 {
		if matchers, err = orderedLabels(kept, false); err != nil {
			return rangeQuery{}, err
		}
	}
	conditions, err := checkWhere(request.Where, matchers)
	if err != nil {
		return rangeQuery{}, err
	}
	limits, err := narrowLimits(request.Limits, s.opts.Limits)
	if err != nil {
		return rangeQuery{}, err
	}
	cutoff := s.cutoff()
	return rangeQuery{
		matchers: matchers, conditions: conditions, limits: limits,
		origin: origin, from: max(origin, cutoff), to: to, cutoff: cutoff,
	}, nil
}

// bounds is a range's [from, to) in unix milliseconds: Since back from the
// store's clock, or From as given, to To, whose zero is the open end
//
//	now 12:00, Since 1h            →  [11:00, open)
//	From 09:00, To 10:00           →  [09:00, 10:00)
func (s *Store) bounds(request Range) (from, to int64, err error) {
	from, to = request.From, request.To
	if request.Since != 0 {
		if request.Since < 0 || request.From != 0 {
			return 0, 0, fmt.Errorf("%w: a range starts Since before now or at From, not both", ErrInvalid)
		}
		from = earlier(s.now().UnixMilli(), request.Since.Milliseconds())
	}
	if to == 0 {
		to = math.MaxInt64
	}
	if to < from {
		return 0, 0, fmt.Errorf("%w: inverted time range", ErrInvalid)
	}
	return from, to, nil
}

// yieldResults hands each nonempty series to yield, with its samples inside
// the range and within the output limit across every series.
func (s *Store) yieldResults(
	ctx context.Context, query rangeQuery, reads []seriesRead, yield func(Result) error,
) error {
	output := 0
	for _, read := range reads {
		result := Result{Series: publicSeries(read.series.labels, read.series.kind)}
		err := s.eachSample(ctx, read, func(point Sample) error {
			if point.At < query.from || point.At >= query.to {
				return nil
			}
			if output == query.limits.OutputSamples {
				return limit(LimitOutputSamples, output+1, query.limits.OutputSamples)
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
