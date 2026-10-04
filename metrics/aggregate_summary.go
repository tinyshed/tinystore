package metrics

import (
	"context"
	"fmt"
	"math"
)

// aggregateSelection says which blocks an aggregate answers from their
// summaries: one inside a bucket of the range, and, when it counts steps, one
// before the range, of which only the last sample counts
type aggregateSelection struct {
	origin, width, from int64
	steps               bool
}

func (a aggregateSelection) complete(block storedBlock, to int64) bool {
	if a.steps && block.head.End < a.from {
		return block.summary.valid
	}
	if block.summary.exactSum == nil || block.head.Start < a.from || block.head.End >= to {
		return false
	}
	start, end := bucketEdges(a.origin, to, block.head.Start, a.width)
	return block.head.End < end && block.head.Start >= start
}

func (a *aggregation) foldRead(ctx context.Context, read seriesRead, series *seriesBuckets) error {
	for _, block := range read.blocks {
		if err := ctx.Err(); err != nil {
			return err
		}
		if block.summarized {
			if err := series.addBlock(block); err != nil {
				return err
			}
		} else if err := a.store.visitBlock(ctx, block, series.add); err != nil {
			return err
		}
	}
	return a.store.visitHead(ctx, read.head, series.add)
}

func (b *seriesBuckets) addBlock(block storedBlock) error {
	if b.kind == Counter && block.summary.min < 0 {
		return ErrCounterValue
	}
	if b.hasPrevious && block.head.Start <= b.previousAt {
		return fmt.Errorf("%w: overlapping summary samples", ErrCorrupt)
	}
	b.previousAt, b.hasPrevious = block.head.End, true
	if block.head.End < b.from { // before the range: only its last sample counts, for the first step
		return b.carry(block.summary.last, true)
	}
	start, end := bucketEdges(b.origin, b.to, block.head.Start, b.width)
	if err := b.begin(start, end); err != nil {
		return err
	}
	if err := b.current.mergeSummary(block, b.op); err != nil {
		return err
	}
	return b.carry(block.summary.last, false)
}

func (b *bucketAccumulator) mergeSummary(block storedBlock, op AggregateOp) error {
	summary := block.summary
	if block.head.Count > math.MaxInt-b.count {
		return fmt.Errorf("%w: aggregate summary count", ErrLimit)
	}
	if b.count == 0 {
		b.minimum, b.maximum = summary.min, summary.max
	} else {
		b.minimum = math.Min(b.minimum, summary.min)
		b.maximum = math.Max(b.maximum, summary.max)
	}
	switch op {
	case AggregateSum, AggregateAvg:
		if err := exactValue(summary.exactSum, &b.current); err != nil {
			return err
		}
		b.exact.Add(&b.exact, &b.current)
	case AggregateIncrease, AggregateRate:
		if err := b.mergeIncrease(block); err != nil {
			return err
		}
	case AggregateDelta:
		if err := b.mergeEnds(block); err != nil {
			return err
		}
	case AggregateFirst:
		if b.count == 0 {
			b.firstValue = block.head.First
			if err := finiteUnits(b.firstValue, &b.first); err != nil {
				return err
			}
		}
	case AggregateLast:
		b.previousValue = block.summary.last
		if err := finiteUnits(b.previousValue, &b.previous); err != nil {
			return err
		}
	}
	b.count += block.head.Count
	return nil
}

// mergeEnds keeps a delta's ends: the bucket's first sample, its first
// block's, and its last, each block's in turn.
func (b *bucketAccumulator) mergeEnds(block storedBlock) error {
	if b.count == 0 && !b.stepFrom {
		if err := finiteUnits(block.head.First, &b.first); err != nil {
			return err
		}
	}
	return finiteUnits(block.summary.last, &b.previous)
}

func (b *bucketAccumulator) mergeIncrease(block storedBlock) error {
	if err := finiteUnits(block.head.First, &b.current); err != nil {
		return err
	}
	if b.count > 0 || b.stepFrom {
		if block.head.First < b.previousValue {
			b.exact.Add(&b.exact, &b.current)
			b.resets++
		} else {
			b.difference.Sub(&b.current, &b.previous)
			b.exact.Add(&b.exact, &b.difference)
		}
	}
	if err := exactValue(block.summary.exactIncrease, &b.current); err != nil {
		return err
	}
	if b.current.Sign() < 0 {
		return fmt.Errorf("%w: negative exact increase", ErrCorrupt)
	}
	b.exact.Add(&b.exact, &b.current)
	b.resets += int(block.summary.resets)
	b.previousValue = block.summary.last
	return finiteUnits(b.previousValue, &b.previous)
}
