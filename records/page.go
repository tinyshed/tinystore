package records

import (
	"cmp"
	"container/heap"
	"context"
	"fmt"
	"math"
	"slices"
	"strings"
	"time"

	"github.com/tinyshed/tinystore"
)

// buildPage decodes what was fetched, keeps the first matches in page order
// and ends the page where its limit, its budget or a shared timestamp says
func (s *Store) buildPage(ctx context.Context, q *checkedQuery, f fetched) (Page, error) {
	d := newDecoder(s.unpack)
	chosen := selection{limit: q.limit, newest: q.asked.Newest, kept: rankedHeap{newest: q.asked.Newest}}
	schemas := map[int64]*schema{}
	for i := range f.sources {
		if err := ctx.Err(); err != nil {
			return Page{}, err
		}
		source := &f.sources[i]
		records, err := s.sourceRecords(d, q, source, f.segments, schemas)
		if err != nil {
			return Page{}, err
		}
		for row := range records {
			chosen.offer(&records[row], source, row)
		}
	}
	return chosen.page(q, f)
}

// sourceRecords decodes one block or head row and returns the records it
// holds that match, in the order it stores them
func (s *Store) sourceRecords(
	d *decoder, q *checkedQuery, src *source, segments map[int64][]byte, schemas map[int64]*schema,
) ([]Record, error) {
	if !src.block {
		records, err := d.parseHeadRow(s.streams.name(src.stream), src.body)
		if err != nil {
			return nil, fmt.Errorf("records: head row %d: %w", src.id, err)
		}
		return slices.DeleteFunc(records, func(r Record) bool { return !q.matches(&r) }), nil
	}
	records, err := d.blockMatches(q, src, segments, schemas)
	if err != nil {
		return nil, fmt.Errorf("records: segment %d, block %d: %w", src.segment, src.id, err)
	}
	return records, nil
}

func (d *decoder) blockMatches(
	q *checkedQuery, src *source, segments map[int64][]byte, schemas map[int64]*schema,
) ([]Record, error) {
	s, ok := schemas[src.segment]
	if !ok {
		var err error
		if s, err = d.parseSchema(segments[src.segment]); err != nil {
			return nil, err
		}
		schemas[src.segment] = s
	}
	block, err := d.openBlock(s, src.body)
	if err != nil {
		return nil, err
	}
	keep, err := d.keepRows(block, q)
	if err != nil || !slices.Contains(keep, true) {
		return nil, err
	}
	records, err := d.records(block, func(row int) bool { return keep[row] })
	return slices.DeleteFunc(records, func(r Record) bool { return !q.matches(&r) }), err
}

// ranked is a record and its place in page order: its time, then where it is
// stored, sealed segments before the head, then its row
type ranked struct {
	record Record
	at     int64
	source int64
	id     int64
	row    int
}

func rankedRecord(record *Record, src *source, row int) ranked {
	place := int64(math.MaxInt64)
	if src.block {
		place = src.segment
	}
	return ranked{at: record.At.UnixNano(), source: place, id: src.id, row: row}
}

func compareRanked(a, b *ranked) int {
	return cmp.Or(cmp.Compare(a.at, b.at), cmp.Compare(a.source, b.source), cmp.Compare(a.id, b.id),
		cmp.Compare(a.row, b.row))
}

// selection keeps the first limit records in page order, and the time of the
// best one it had to let go: records at that time cannot all be returned, so
// the page ends before it
type selection struct {
	limit     int
	newest    bool
	kept      rankedHeap
	dropped   bool
	droppedAt int64
}

// offer keeps a record that belongs on the page; a kept record is detached
// from the block it was decoded from, so the page holds its records and not
// the columns they came from
func (s *selection) offer(record *Record, src *source, row int) {
	r := rankedRecord(record, src, row)
	if len(s.kept.items) < s.limit {
		r.record = detached(record)
		heap.Push(&s.kept, r)
		return
	}
	worst := &s.kept.items[0]
	if s.before(&r, worst) {
		s.drop(worst.at)
		r.record = detached(record)
		s.kept.items[0] = r
		heap.Fix(&s.kept, 0)
		return
	}
	s.drop(r.at)
}

// detached is a record holding copies of what it shares with its block: the
// body, the lists and the values in them; keys and names are the segment's
func detached(r *Record) Record {
	copied := *r
	if r.Level != nil {
		copied.Level = new(*r.Level)
	}
	if r.Body != nil {
		copied.Body = new(strings.Clone(*r.Body))
	}
	copied.Context, copied.Attrs = detachedFields(r.Context), detachedFields(r.Attrs)
	return copied
}

func detachedFields(fields []Field) []Field {
	if fields == nil {
		return nil
	}
	copied := make([]Field, len(fields))
	for i, field := range fields {
		copied[i] = Field{Key: field.Key, Value: strings.Clone(field.Value)}
	}
	return copied
}

func (s *selection) before(a, b *ranked) bool {
	if s.newest {
		return compareRanked(a, b) > 0
	}
	return compareRanked(a, b) < 0
}

func (s *selection) drop(at int64) {
	switch {
	case !s.dropped:
		s.dropped, s.droppedAt = true, at
	case s.newest:
		s.droppedAt = max(s.droppedAt, at)
	default:
		s.droppedAt = min(s.droppedAt, at)
	}
}

// page ends before the first time it cannot return whole: the budget's edge,
// or the time of the best record the limit let go
//
//	oldest first, limit 3    kept .1 .2 .2   let go .2 .3   → page .1, next from .2
func (s *selection) page(q *checkedQuery, f fetched) (Page, error) {
	bounded, bound := f.cut, f.edge
	if s.dropped {
		bound = s.tighter(bounded, bound, s.droppedAt)
		bounded = true
	}
	records := s.sorted(bounded, bound)
	page := Page{Records: records, Next: q.asked}
	if !bounded {
		return page, nil
	}
	if len(records) == 0 && (s.stuck(q, bound) || (s.dropped && bound == s.droppedAt)) {
		return Page{}, fmt.Errorf("%w: more records at %s than a page of %d or its budget holds",
			tinystore.ErrLimit, time.Unix(0, bound).UTC(), q.limit)
	}
	page.More = true
	if s.newest {
		page.Next.To = time.Unix(0, bound).UTC().Add(time.Nanosecond)
	} else {
		page.Next.From = time.Unix(0, bound).UTC()
	}
	return page, nil
}

// tighter is the bound nearer the page's start
func (s *selection) tighter(bounded bool, bound, at int64) int64 {
	switch {
	case !bounded:
		return at
	case s.newest:
		return max(bound, at)
	}
	return min(bound, at)
}

// stuck is a page that could not move past its start: the next page would ask
// for the same records again
func (s *selection) stuck(q *checkedQuery, bound int64) bool {
	if s.newest {
		return bound >= q.last
	}
	return bound <= q.first
}

// sorted is what the page returns: the kept records strictly before the bound,
// in page order
func (s *selection) sorted(bounded bool, bound int64) []Record {
	items := slices.Clone(s.kept.items)
	slices.SortFunc(items, func(a, b ranked) int {
		if s.newest {
			return compareRanked(&b, &a)
		}
		return compareRanked(&a, &b)
	})
	records := make([]Record, 0, len(items))
	for _, item := range items {
		if bounded && ((!s.newest && item.at >= bound) || (s.newest && item.at <= bound)) {
			continue
		}
		records = append(records, item.record)
	}
	return records
}

// rankedHeap puts the record last in page order on top, the one to let go first
type rankedHeap struct {
	items  []ranked
	newest bool
}

func (h *rankedHeap) Len() int { return len(h.items) }

func (h *rankedHeap) Less(i, j int) bool {
	if h.newest {
		return compareRanked(&h.items[i], &h.items[j]) < 0
	}
	return compareRanked(&h.items[i], &h.items[j]) > 0
}

func (h *rankedHeap) Swap(i, j int) { h.items[i], h.items[j] = h.items[j], h.items[i] }

func (h *rankedHeap) Push(x any) {
	if item, ok := x.(ranked); ok {
		h.items = append(h.items, item)
	}
}

func (h *rankedHeap) Pop() any {
	last := h.items[len(h.items)-1]
	h.items = h.items[:len(h.items)-1]
	return last
}
