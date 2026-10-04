package metrics

import (
	"context"
	"errors"

	"github.com/tinyshed/tinystore"
)

// Plan is what a Read or an Aggregate would spend of its limits, found in one
// snapshot from the series, their block directories and their heads, with no
// block's payload fetched and no sample decoded. Each use stands beside the
// limit it is held to, and Stops is the limit the call would reach, nil when
// it fits; the plan stops counting there, as the call would stop working.
//
//	Series 3, Blocks 40, Summarized 36, PayloadBytes 12288, DecodedSamples 960
//	  → an hourly aggregate answers 36 blocks from their summaries, decodes 4
//
// The samples a Read answers are not known before its blocks are decoded, so
// a plan does not count them.
type Plan struct {
	Series, Blocks, Summarized, PayloadBytes, DecodedSamples int
	Limits                                                   Limits
	Stops                                                    *tinystore.LimitError
}

// ExplainRead is the Plan of Read(ctx, request).
func (s *Store) ExplainRead(ctx context.Context, request Range) (Plan, error) {
	query, err := s.checkRange(request)
	if err != nil {
		return Plan{}, err
	}
	return s.explain(ctx, query)
}

// ExplainAggregate is the Plan of Aggregate(ctx, request), whose whole blocks
// inside one bucket are answered from their summaries.
func (s *Store) ExplainAggregate(ctx context.Context, request AggregateRequest) (Plan, error) {
	query, err := s.checkAggregate(request)
	if err != nil {
		return Plan{}, err
	}
	if query.from >= query.to {
		return Plan{Limits: query.limits}, nil
	}
	withLookback(request, &query)
	return s.explain(ctx, query)
}

func (s *Store) explain(ctx context.Context, query rangeQuery) (Plan, error) {
	release, err := s.admit(ctx, s.readSlots)
	if err != nil {
		return Plan{}, err
	}
	defer release()

	plan := Plan{Limits: query.limits}
	if query.from >= query.to {
		return plan, nil
	}
	query.planOnly = true
	_, budget, err := s.fetchSnapshotSpending(ctx, query)
	var reached *tinystore.LimitError
	switch {
	case errors.As(err, &reached):
		plan.Stops = reached
	case err != nil:
		return Plan{}, err
	}
	plan.Series, plan.Blocks, plan.Summarized = budget.series, budget.blocks, budget.summarized
	plan.PayloadBytes, plan.DecodedSamples = budget.bytes, budget.decoded
	return plan, nil
}
