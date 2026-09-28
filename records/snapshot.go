package records

import (
	"cmp"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"math"
	"slices"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// source is one block or head row a page may take records from
type source struct {
	block       bool
	id, segment int64 // a head row has no segment
	stream      int64
	first, last int64
	count, size int
	body        []byte
}

// fetched is what one page decodes, copied out of one read transaction. When
// the budget stopped before the last candidate, edge is where the candidates
// taken stop covering every record: the first time of the one left, or its
// last time for a page of the newest.
type fetched struct {
	sources  []source
	segments map[int64]segmentRow
	cut      bool
	edge     int64
	bytes    int
}

// fetchSnapshot copies every row a page may need out of one read transaction,
// which ends before anything is decoded
func (s *Store) fetchSnapshot(ctx context.Context, q *checkedQuery) (fetched, error) {
	ctx, cancel := context.WithTimeout(ctx, snapshotTimeout)
	defer cancel()
	var out fetched
	err := s.file.ViewPrepared(ctx, func(tx sqlite.Reader) error {
		read := snapshotRead{tx: tx, query: q, names: &s.streams, spans: &s.spans}

		candidates, err := read.candidates(ctx)
		if err != nil {
			return err
		}

		out, err = read.withinBudget(ctx, candidates)
		return err
	})
	if err != nil {
		return fetched{}, fmt.Errorf("records: read: %w", err)
	}
	return out, nil
}

// segmentRow is a segment row its blocks are decoded with, and the times it holds
type segmentRow struct {
	first, last int64
	body        []byte
}

// snapshotRead is what every fetch inside one read transaction shares
type snapshotRead struct {
	tx    sqlite.Reader
	query *checkedQuery
	names *streams
	spans *blockSpans
}

const (
	blockCandidatesBase = `
		select id, segment, stream, first_at, last_at, count, size from blocks
		where span = ? and last_at between ? and ? and first_at between ? and ?
			and (? = 0 or levels & ? != 0)`
	headCandidatesBase = `
		select id, 0, stream, first_at, last_at, count, size from heads
		where last_at >= ? and first_at <= ? and (? = 0 or levels & ? != 0)`
	oldestCandidates = ` and (first_at, id) > (?, ?) order by first_at, id limit cast(? as integer)`
	newestCandidates = ` and (last_at, id) < (?, ?) order by last_at desc, id desc limit cast(? as integer)`
	candidateChunk   = 256
)

// candidates are the blocks and head rows the time index and level masks
// cannot rule out, less the blocks their segment's keys or their blooms rule
// out, in the order a page takes them. The head rows are read first: the
// snapshot begins with them, and the spans are known from then on.
func (r *snapshotRead) candidates(ctx context.Context) ([]source, error) {
	heads, err := r.headCandidates(ctx)
	if err != nil {
		return nil, err
	}
	blocks, err := r.blockCandidates(ctx)
	if err != nil {
		return nil, err
	}
	all := append(blocks, heads...)
	sortSources(all, r.query.asked.Newest)
	return all, nil
}

func (r *snapshotRead) headCandidates(ctx context.Context) ([]source, error) {
	q := r.query
	return r.pagedCandidates(ctx, false, headCandidatesBase,
		[]any{q.first, q.last, q.levels, q.levels})
}

// blockCandidates asks the time index once for each span a block has had
func (r *snapshotRead) blockCandidates(ctx context.Context) ([]source, error) {
	q := r.query
	var blocks []source
	for span := range r.spans.each() {
		lastEnd := latestEnd(span, q.last)
		found, err := r.pagedCandidates(ctx, true, blockCandidatesBase,
			[]any{span, q.first, lastEnd, earliestStart(span, q.first), q.last, q.levels, q.levels})
		if err != nil {
			return nil, err
		}
		blocks = append(blocks, found...)
	}
	return blocks, nil
}

// pagedCandidates walks one index in the order a page takes candidates, and
// stops one past the block budget: a page takes no more from any walk, so
// the walks of other spans cannot push a candidate further in
func (r *snapshotRead) pagedCandidates(ctx context.Context, block bool, base string, args []any) ([]source, error) {
	most := r.query.budget.Blocks + 1
	query := base + oldestCandidates
	position, id := int64(math.MinInt64), int64(0)
	if r.query.asked.Newest {
		query = base + newestCandidates
		position, id = math.MaxInt64, math.MaxInt64
	}
	var selected []source
	for len(selected) < most {
		limit := min(candidateChunk, most-len(selected))
		batch, err := r.indexed(ctx, block, query, append(slices.Clone(args), position, id, limit)...)
		if err != nil {
			return nil, err
		}
		if len(batch) == 0 {
			break
		}
		position, id = r.positionOf(batch[len(batch)-1]), batch[len(batch)-1].id
		walked := len(batch)
		if batch, err = r.asked(ctx, block, batch); err != nil {
			return nil, err
		}
		selected = append(selected, batch...)
		if walked < limit {
			break
		}
	}
	return selected[:min(len(selected), most)], nil
}

// positionOf is where a walk continues after a candidate
func (r *snapshotRead) positionOf(candidate source) int64 {
	if r.query.asked.Newest {
		return candidate.last
	}
	return candidate.first
}

// asked keeps the candidates of the streams asked for, less the blocks their
// segment's keys or blooms rule out
func (r *snapshotRead) asked(ctx context.Context, block bool, batch []source) ([]source, error) {
	if block {
		return r.withoutExcluded(ctx, batch)
	}
	if r.query.streams != nil {
		batch = slices.DeleteFunc(batch, func(src source) bool { return !r.query.streams[src.stream] })
	}
	return batch, nil
}

func (r *snapshotRead) indexed(ctx context.Context, block bool, query string, arguments ...any) ([]source, error) {
	rows, err := r.tx.QueryContext(ctx, query, arguments...) //nolint:rowserrcheck // EachRow checks Err
	if err != nil {
		return nil, err
	}
	var found []source
	err = sqlite.EachRow(rows, "candidates", func(rows *sql.Rows) error {
		candidate := source{block: block}
		scanErr := rows.Scan(&candidate.id, &candidate.segment, &candidate.stream, &candidate.first,
			&candidate.last, &candidate.count, &candidate.size)
		if scanErr != nil {
			return scanErr
		}
		if candidate.count < 1 || candidate.count > maxBlockRecords || candidate.size < 1 ||
			candidate.size > maxSegmentInput || candidate.first > candidate.last {
			return r.damagedCandidate(candidate, "indexed count, size or time")
		}
		found = append(found, candidate)
		return nil
	})
	return found, err
}

func (r *snapshotRead) damagedCandidate(candidate source, reason string) error {
	damage := Damage{Stream: r.names.name(candidate.stream), From: timeOf(candidate.first), To: timeOf(candidate.last)}
	if candidate.block {
		damage.Segment = candidate.segment
	} else {
		damage.HeadRow = candidate.id
	}
	return damageOf(damage, corrupt(reason))
}

// sortSources puts candidates in the order a page takes them: by first time,
// or by last time falling for the newest first
func sortSources(sources []source, newest bool) {
	slices.SortFunc(sources, func(a, b source) int {
		if newest {
			return cmp.Or(cmp.Compare(b.last, a.last), cmp.Compare(b.id, a.id))
		}
		return cmp.Or(cmp.Compare(a.first, b.first), cmp.Compare(a.id, b.id))
	})
}

func (r *snapshotRead) withoutExcluded(ctx context.Context, blocks []source) ([]source, error) {
	segments := map[int64]bool{}
	kept := blocks[:0]
	for _, block := range blocks {
		if r.query.streams != nil && !r.query.streams[block.stream] {
			continue
		}
		keep, known := segments[block.segment]
		var err error
		if !known {
			if keep, err = r.segmentMayMatch(ctx, block.segment); err != nil {
				return nil, err
			}
			segments[block.segment] = keep
		}
		if keep {
			if keep, err = r.bloomsMayMatch(ctx, block.id); err != nil {
				return nil, err
			}
		}
		if keep {
			kept = append(kept, block)
		}
	}
	return kept, nil
}

const selectSegmentKey = `select count(*) from segment_keys where segment = ? and kind = ? and key = ?`

// segmentMayMatch asks a segment's keys for one of the names and for every
// attribute and context key the query names
func (r *snapshotRead) segmentMayMatch(ctx context.Context, segment int64) (bool, error) {
	asked := &r.query.asked
	if len(asked.Names) > 0 {
		found := false
		for _, name := range asked.Names {
			has, err := r.hasKey(ctx, segment, keyName, name)
			if err != nil {
				return false, err
			}
			found = found || has
		}
		if !found {
			return false, nil
		}
	}
	attrs, err := r.hasKeys(ctx, segment, keyAttr, asked.Attrs)
	if err != nil || !attrs {
		return false, err
	}
	return r.hasKeys(ctx, segment, keyContext, asked.Context)
}

func (r *snapshotRead) hasKeys(ctx context.Context, segment int64, kind byte, fields []Field) (bool, error) {
	for _, field := range fields {
		if has, err := r.hasKey(ctx, segment, kind, field.Key); err != nil || !has {
			return false, err
		}
	}
	return true, nil
}

func (r *snapshotRead) hasKey(ctx context.Context, segment int64, kind byte, key string) (bool, error) {
	var count int
	err := sqlite.QueryRow(ctx, r.tx, selectSegmentKey, segment, kind, key).Scan(&count)
	return count > 0, err
}

const (
	selectTraceBloom = `select bloom from block_traces where block = ?`
	selectAttrBloom  = `select bloom from block_filters where block = ? and key = ?`
)

// bloomsMayMatch rules a block out when its trace bloom lacks the trace, or
// an attribute's bloom lacks the value; a block without a bloom for a key may
// still hold it
func (r *snapshotRead) bloomsMayMatch(ctx context.Context, block int64) (bool, error) {
	asked := &r.query.asked
	if asked.TraceID != (TraceID{}) {
		bloom, found, err := r.bloom(ctx, selectTraceBloom, block)
		if err != nil || !found || !bloomMayHold(bloom, string(asked.TraceID[:])) {
			return false, err
		}
	}
	for _, field := range asked.Attrs {
		bloom, found, err := r.bloom(ctx, selectAttrBloom, block, field.Key)
		if err != nil || (found && !bloomMayHold(bloom, field.Value)) {
			return false, err
		}
	}
	return true, nil
}

func (r *snapshotRead) bloom(ctx context.Context, query string, args ...any) ([]byte, bool, error) {
	var bloom []byte
	err := sqlite.QueryRow(ctx, r.tx, query, args...).Scan(&bloom)
	if errors.Is(err, sql.ErrNoRows) {
		return nil, false, nil
	}
	return bloom, err == nil, err
}

const (
	selectSegmentSize = `select length(body) from segments where id = ?`
	selectSegmentBody = `select first_at, last_at, body from segments where id = ?`
	selectBlockBody   = `select body from blocks where id = ?`
	selectHeadBody    = `select body from heads where id = ?`
)

// withinBudget fetches candidates in page order while the budget lasts; the
// first one it cannot afford is the page's edge
func (r *snapshotRead) withinBudget(ctx context.Context, candidates []source) (fetched, error) {
	out := fetched{segments: map[int64]segmentRow{}}
	spent := Budget{}
	for _, candidate := range candidates {
		segmentSize, err := r.segmentSize(ctx, candidate, out.segments)
		if err != nil {
			return out, err
		}
		if r.enough(out.sources, candidate) || !r.affords(spent, candidate, segmentSize) {
			out.cut, out.edge = true, r.edgeOf(candidate)
			break
		}
		if err = r.fetch(ctx, &candidate, segmentSize, out.segments); err != nil {
			return out, err
		}
		spent.Blocks, spent.Decoded = spent.Blocks+1, spent.Decoded+candidate.count
		spent.Bytes += candidate.size + segmentSize
		out.sources = append(out.sources, candidate)
	}
	out.bytes = spent.Bytes
	if out.cut && len(out.sources) == 0 {
		return out, fmt.Errorf("%w: records at %s need more than the budget %+v allows",
			tinystore.ErrLimit, time.Unix(0, out.edge).UTC(), r.query.budget)
	}
	return out, nil
}

// segmentSize is what the candidate's segment row adds, when it is not fetched yet
func (r *snapshotRead) segmentSize(ctx context.Context, candidate source, segments map[int64]segmentRow) (int, error) {
	if !candidate.block {
		return 0, nil
	}
	if _, fetched := segments[candidate.segment]; fetched {
		return 0, nil
	}
	var size int
	err := sqlite.QueryRow(ctx, r.tx, selectSegmentSize, candidate.segment).Scan(&size)
	if errors.Is(err, sql.ErrNoRows) {
		found := Damage{
			Stream: r.names.name(candidate.stream), Segment: candidate.segment,
			From: timeOf(candidate.first), To: timeOf(candidate.last),
		}
		gone := corrupt(fmt.Sprintf("block %d names segment %d, which is gone", candidate.id, candidate.segment))
		return 0, damageOf(found, gone)
	}
	return size, err
}

func (r *snapshotRead) affords(spent Budget, candidate source, segmentSize int) bool {
	budget := r.query.budget
	return spent.Blocks < budget.Blocks && candidate.count <= budget.Decoded-spent.Decoded &&
		candidate.size+segmentSize <= budget.Bytes-spent.Bytes
}

// enough stops a page once the candidates taken hold, spread evenly over their
// spans, a page's worth of records before the next candidate begins: the page
// ends there, and what lies past it is the next page's to read. A query that
// filters rows cannot tell how many will match, so it takes what its budget
// allows instead.
//
//	limit 100, oldest first    taken [0 s, 10 s) 1000 records    next begins at 2 s
//	expected before 2 s: 1000 × 2/10 = 200 ≥ 100                  → the page ends at 2 s
func (r *snapshotRead) enough(taken []source, next source) bool {
	q := r.query
	if len(taken) == 0 || q.filtersRows() {
		return false
	}
	bound := r.edgeOf(next)
	if (!q.asked.Newest && bound <= q.first) || (q.asked.Newest && bound >= q.last) {
		return false
	}
	expected := 0.0
	for _, candidate := range taken {
		expected += q.expectedBefore(candidate, bound)
	}
	return expected >= float64(q.limit)
}

func (r *snapshotRead) edgeOf(candidate source) int64 {
	if r.query.asked.Newest {
		return candidate.last
	}
	return candidate.first
}

func (r *snapshotRead) fetch(ctx context.Context, candidate *source, segmentSize int,
	segments map[int64]segmentRow,
) error {
	if !candidate.block {
		if err := sqlite.QueryRow(ctx, r.tx, selectHeadBody, candidate.id).Scan(&candidate.body); err != nil {
			return err
		}
		if len(candidate.body) != candidate.size {
			return r.damagedCandidate(*candidate, "head body size differs from index")
		}
		return nil
	}
	if _, fetched := segments[candidate.segment]; !fetched {
		var row segmentRow
		err := sqlite.QueryRow(ctx, r.tx, selectSegmentBody, candidate.segment).Scan(&row.first, &row.last, &row.body)
		if err != nil {
			return err
		}
		if len(row.body) != segmentSize {
			return r.damagedCandidate(*candidate, "segment body size differs from index")
		}
		segments[candidate.segment] = row
	}
	if err := sqlite.QueryRow(ctx, r.tx, selectBlockBody, candidate.id).Scan(&candidate.body); err != nil {
		return err
	}
	if len(candidate.body) != candidate.size {
		return r.damagedCandidate(*candidate, "block body size differs from index")
	}
	return nil
}
