package records

import (
	"context"
	"encoding/json"
	"fmt"
	"iter"
	"math"
	"slices"

	"github.com/tinyshed/tinystore"
)

// Scan returns one page of the records a query selects, in event-time order:
// oldest first, or newest first when it asks. A page ends where its limit or
// its budget ran out, never inside one timestamp; Page.More says so, and
// Page.Next asks for what follows. Nothing is decoded while the snapshot the
// page was read from is held.
func (s *Store) Scan(ctx context.Context, query Query) (Page, error) {
	release, err := s.admitTo(ctx, s.reads)
	if err != nil {
		return Page{}, err
	}
	defer release()

	checked, err := s.checkQuery(query)
	if err != nil || checked.nothing {
		return Page{Next: query}, err
	}

	unreserve, err := s.reserve(ctx, checked.reservation)
	if err != nil {
		return Page{}, err
	}
	defer unreserve()

	rows, err := s.fetchSnapshot(ctx, &checked)
	if err != nil {
		return Page{}, s.noted(err)
	}

	page, err := s.buildPage(ctx, &checked, rows)
	if err != nil {
		return Page{}, s.noted(err)
	}
	s.countRead(len(rows.sources), rows.bytes)
	return page, nil
}

// All walks every record a query selects, a page at a time, holding no
// snapshot between pages; an error ends the walk after it is yielded.
func (s *Store) All(ctx context.Context, query Query) iter.Seq2[Record, error] {
	return func(yield func(Record, error) bool) {
		for {
			page, err := s.Scan(ctx, query)
			if err != nil {
				yield(Record{}, err)
				return
			}
			for _, record := range page.Records {
				if !yield(record, nil) {
					return
				}
			}
			if !page.More {
				return
			}
			query = page.Next
		}
	}
}

// checkedQuery is a query the engine can run, its conditions resolved.
type checkedQuery struct {
	asked Query
	// first and last are the range as nanoseconds, both ends included, the
	// first clipped by one retention cutoff.
	first, last int64
	streams     map[int64]bool // nil: every stream
	levels      int64          // the level bits a block must share, zero for any
	limit       int
	budget      Budget
	nothing     bool // no record can match: an empty range, or none of the streams exists
}

func (s *Store) checkQuery(query Query) (checkedQuery, error) {
	query, err := s.since(query)
	if err != nil {
		return checkedQuery{asked: query}, err
	}
	q := checkedQuery{asked: query}
	if q.limit, err = checkLimit(query.Limit); err != nil {
		return q, err
	}
	if q.budget, err = s.opts.Budget.narrow(query.Budget); err != nil {
		return q, err
	}
	if err = checkConditions(query); err != nil {
		return q, err
	}
	if q.first, q.last, err = s.queryRange(query); err != nil {
		return q, err
	}
	if query.MinLevel != nil {
		q.levels = levelsFrom(int64(*query.MinLevel))
	}
	q.streams = s.knownStreams(query.Streams)
	q.nothing = q.first > q.last || (query.Streams != nil && len(q.streams) == 0)
	return q, nil
}

func checkLimit(limit int) (int, error) {
	switch {
	case limit < 0 || limit > maxLimit:
		return 0, fmt.Errorf("%w: a limit of %d records, not 1 to %d", tinystore.ErrInvalid, limit, maxLimit)
	case limit == 0:
		return defaultLimit, nil
	}
	return limit, nil
}

// checkConditions refuses a value no stored value could equal, which is a
// mistake more often than a question: an unquoted string is not JSON
func checkConditions(query Query) error {
	for _, field := range slices.Concat(query.Attrs, query.Context) {
		if !json.Valid([]byte(field.Value)) {
			return fmt.Errorf("%w: the value of %q is not JSON: %.64q", tinystore.ErrInvalid, field.Key, field.Value)
		}
	}
	if query.MinLevel != nil && (*query.MinLevel < minLevel || *query.MinLevel > maxLevel) {
		return fmt.Errorf("%w: level %d is past 32 bits", tinystore.ErrInvalid, *query.MinLevel)
	}
	return nil
}

// since resolves a query over the last Since into the From it starts at, so
// that each page after the first continues the same range
func (s *Store) since(query Query) (Query, error) {
	if query.Since == 0 {
		return query, nil
	}
	if query.Since < 0 || !query.From.IsZero() {
		return query, fmt.Errorf("%w: a range starts Since before now or at From, not both", tinystore.ErrInvalid)
	}
	query.From, query.Since = s.now().Add(-query.Since), 0
	return query, nil
}

// queryRange turns [From, To) into both ends included; a zero From or To
// leaves that end open, and retention's cutoff moves the first end forward
func (s *Store) queryRange(query Query) (first, last int64, err error) {
	if !query.From.IsZero() && !query.To.IsZero() && query.To.Before(query.From) {
		return 0, 0, fmt.Errorf("%w: the range ends before it starts", tinystore.ErrInvalid)
	}
	first, last = math.MinInt64, math.MaxInt64
	if !query.From.IsZero() {
		first = unixNanos(query.From)
	}
	if !query.To.IsZero() {
		to := unixNanos(query.To)
		if to == math.MinInt64 {
			return 1, 0, nil
		}
		last = to - 1
	}
	return max(first, s.cutoff()), last, nil
}

func (s *Store) cutoff() int64 {
	return unixNanos(s.now().Add(-s.opts.Retention))
}

// knownStreams resolves names to ids; a name no record ever had matches nothing
func (s *Store) knownStreams(names []string) map[int64]bool {
	if names == nil {
		return nil
	}
	ids := map[int64]bool{}
	for _, name := range names {
		if id, ok := s.streams.id(name); ok {
			ids[id] = true
		}
	}
	return ids
}

// reservation is the fetched bytes, one decoded block and the records a page keeps
func (q *checkedQuery) reservation() int64 {
	return int64(q.budget.Bytes) + blockReservation + int64(q.limit)*recordReservation
}

// matches checks a whole record; a block's columns have already ruled out most
// rows, so this is the one test every returned record has passed
func (q *checkedQuery) matches(r *Record) bool {
	at, asked := r.At.UnixNano(), &q.asked
	switch {
	case at < q.first || at > q.last:
		return false
	case len(asked.Names) > 0 && !slices.Contains(asked.Names, r.Name):
		return false
	case asked.MinLevel != nil && (r.Level == nil || *r.Level < *asked.MinLevel):
		return false
	case asked.TraceID != (TraceID{}) && r.TraceID != asked.TraceID:
		return false
	}
	for _, want := range asked.Attrs {
		if !slices.Contains(r.Attrs, want) {
			return false
		}
	}
	for _, want := range asked.Context {
		if !slices.Contains(r.Context, want) {
			return false
		}
	}
	return true
}

// filtersRows is a query whose conditions only a record's own values settle
func (q *checkedQuery) filtersRows() bool {
	asked := &q.asked
	return len(asked.Names) > 0 || asked.MinLevel != nil || asked.TraceID != (TraceID{}) ||
		len(asked.Attrs) > 0 || len(asked.Context) > 0
}

// expectedBefore is how many of a candidate's records fall on this page's side
// of bound, were they spread evenly over its span. It only decides when to stop
// fetching, never what a page returns.
func (q *checkedQuery) expectedBefore(candidate source, bound int64) float64 {
	low, high := float64(max(candidate.first, q.first)), float64(min(candidate.last, q.last))
	if q.asked.Newest {
		low = max(low, float64(bound)+1)
	} else {
		high = min(high, float64(bound)-1)
	}
	span := float64(candidate.last) - float64(candidate.first) + 1
	if high < low {
		return 0
	}
	return float64(candidate.count) * min(1, (high-low+1)/span)
}

func (s *Store) countRead(blocks, bytes int) {
	s.queries.Add(1)
	s.readBlocks.Add(unsigned(blocks))
	s.readBytes.Add(unsigned(bytes))
}
