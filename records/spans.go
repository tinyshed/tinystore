package records

import (
	"context"
	"database/sql"
	"fmt"
	"iter"
	"math"
	"math/bits"
	"sync/atomic"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// spanOf files a block by how wide its times are: it ends less than 2^span
// nanoseconds after it begins, so a read ending at t finds it among the
// blocks of its span that end before t + 2^span, and walks no other
//
//	half a second → 29      an hour → 42      two weeks → 51
func spanOf(first, last int64) int {
	return min(bits.Len64(distance(first, last)), 63)
}

// latestEnd is the latest a block of a span may end and still begin by last
func latestEnd(span int, last int64) int64 {
	if span >= 63 {
		return math.MaxInt64
	}
	width := int64(1)<<span - 1
	if last > math.MaxInt64-width {
		return math.MaxInt64
	}
	return last + width
}

// earliestStart is the earliest a block of a span may begin and still end by
// first
func earliestStart(span int, first int64) int64 {
	if span >= 63 {
		return math.MinInt64
	}
	width := int64(1)<<span - 1
	if first < math.MinInt64+width {
		return math.MinInt64
	}
	return first - width
}

// blockSpans holds a bit for every span a block of the file has had: a read
// asks the time index once for each. It only grows, and a span enters it
// before the transaction writing its block commits, so a read that loads it
// once its snapshot has begun knows every span that snapshot holds.
type blockSpans struct {
	mask atomic.Uint64
}

func (b *blockSpans) note(blocks []encodedBlock) {
	var mask uint64
	for _, block := range blocks {
		mask |= 1 << spanOf(block.first, block.last)
	}
	b.mask.Or(mask)
}

// each is the spans a read asks the index for, narrowest first
func (b *blockSpans) each() iter.Seq[int] {
	return func(yield func(int) bool) {
		for mask := b.mask.Load(); mask != 0; mask &= mask - 1 {
			if !yield(bits.TrailingZeros64(mask)) {
				return
			}
		}
	}
}

// the spans the blocks have, one index seek a span
const selectSpans = `
	with recursive spans (span) as (
		select min(span) from blocks
		union all
		select (select min(span) from blocks where span > spans.span) from spans where spans.span is not null
	)
	select span from spans where span is not null`

func (b *blockSpans) load(ctx context.Context, file *sqlite.File) error {
	err := file.View(ctx, func(tx *sql.Tx) error {
		rows, err := tx.QueryContext(ctx, selectSpans) //nolint:rowserrcheck // EachRow checks Err
		if err != nil {
			return err
		}
		return sqlite.EachRow(rows, "block spans", func(rows *sql.Rows) error {
			var span int
			if err := rows.Scan(&span); err != nil {
				return err
			}
			if span < 0 || span > 63 {
				return corrupt("block span")
			}
			b.mask.Or(1 << span)
			return nil
		})
	})
	if err != nil {
		return fmt.Errorf("records: load block spans: %w", err)
	}
	return nil
}
