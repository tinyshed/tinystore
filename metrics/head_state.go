package metrics

import (
	"context"
	"database/sql"
	"fmt"
	"math"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// the tail comes back only when the range needs it and the budget can pay for it
const headQuery = `
	select head_count, head_start, head_end, coalesce(length(tail), 0),
	       case when length(tail) <= ? and head_end >= ? and head_start < ? then tail end
	from series_state
	where series_id = ?`

// fetchHead copies one head out of the read snapshot. With a budget it charges
// the whole encoded tail, then only the samples of the chunks the range needs.
func (s *Store) fetchHead(
	ctx context.Context, tx sqlite.Reader, id, from, to int64, budget *queryBudget,
) (headSnapshot, error) {
	limit := s.opts.MaxHeadBytes
	if budget != nil {
		limit = min(limit, budget.limits.PayloadBytes-budget.bytes)
	}

	head := headSnapshot{seriesID: id}
	var first, last sql.NullInt64
	var size int
	row := sqlite.QueryRowByKey(ctx, tx, headQuery, limit, from, to, id)
	err := row.Scan(&head.count, &first, &last, &size, &head.packed)
	if err != nil {
		return head, fmt.Errorf("read mutable state: %w", err)
	}

	if head.count == 0 {
		if first.Valid || last.Valid || size != 0 {
			return head, fmt.Errorf("%w: empty mutable head", ErrCorrupt)
		}
		return head, nil
	}
	if !first.Valid || !last.Valid || last.Int64 < first.Int64 {
		return head, fmt.Errorf("%w: mutable endpoints", ErrCorrupt)
	}
	head.start, head.end = first.Int64, last.Int64
	if head.end < from || head.start >= to {
		return headSnapshot{seriesID: id, start: head.start, end: head.end}, nil
	}
	if head.count < 0 || head.count > s.opts.MaxHeadSamples || size > s.opts.MaxHeadBytes {
		return head, fmt.Errorf("%w: mutable head capacity", ErrLimit)
	}

	if budget != nil {
		if err = budget.takeBytes(size); err != nil {
			return head, err
		}
	}
	if size == 0 || len(head.packed) != size {
		return head, fmt.Errorf("%w: mutable body missing", ErrCorrupt)
	}
	if budget == nil {
		return head, nil
	}

	head.filtered, head.from, head.to = true, from, to
	if head.chunks, err = s.parseHead(head); err != nil {
		return head, err
	}
	return head, budget.takeSamples(selectedHeadSamples(head.chunks, from, to))
}

// mutablePoints decodes a whole head inside a write transaction.
func (s *Store) mutablePoints(ctx context.Context, tx sqlite.Reader, id int64) ([]Sample, error) {
	head, err := s.fetchHead(ctx, tx, id, math.MinInt64, math.MaxInt64, nil)
	if err != nil {
		return nil, err
	}
	return s.decodeHead(ctx, head)
}

const saveHeadQuery = `update series_state set tail=?, head_count=?, head_start=?, head_end=? where series_id=?`

// saveHead replaces a head after maintenance took samples out of it.
func (s *Store) saveHead(ctx context.Context, tx *sql.Tx, id int64, points []Sample) error {
	packed, err := s.encodeHead(ctx, id, points)
	if err != nil {
		return err
	}

	var first, last sql.NullInt64
	if len(points) > 0 {
		first = sql.NullInt64{Int64: points[0].At, Valid: true}
		last = sql.NullInt64{Int64: points[len(points)-1].At, Valid: true}
	}
	_, err = tx.ExecContext(ctx, saveHeadQuery, packed, len(points), first, last, id)
	if err != nil {
		return fmt.Errorf("replace mutable head: %w", err)
	}
	return nil
}

// ingestState is what one write needs from its series row.
type ingestState struct {
	frontier, nextGC, failedAt sql.NullInt64
	failure                    sql.NullString
	version, maxSeen           int64
	ready                      int
	head                       headSnapshot
}

const ingestStateQuery = `
	select sealed_before, version, max_seen_ts, ready, next_gc_ts, failed_at, failure_reason,
	       head_count, head_start, head_end, coalesce(length(tail), 0),
	       case when length(tail) <= ? then tail end
	from series_state
	where series_id = ?`

func (s *Store) loadIngestState(ctx context.Context, tx sqlite.Writer, id int64) (ingestState, error) {
	state := ingestState{head: headSnapshot{seriesID: id}}
	var first, last sql.NullInt64
	var size int
	err := sqlite.QueryRowByKey(ctx, tx, ingestStateQuery, s.opts.MaxHeadBytes, id).Scan(
		&state.frontier, &state.version, &state.maxSeen, &state.ready, &state.nextGC, &state.failedAt, &state.failure,
		&state.head.count, &first, &last, &size, &state.head.packed,
	)
	if err != nil {
		return state, fmt.Errorf("read ingest state: %w", err)
	}
	if state.failedAt.Valid {
		return state, nil // refused before its head matters
	}

	if state.head.count == 0 {
		if first.Valid || last.Valid || size != 0 {
			return state, fmt.Errorf("%w: empty mutable head", ErrCorrupt)
		}
		return state, nil
	}
	if !first.Valid || !last.Valid || last.Int64 < first.Int64 {
		return state, fmt.Errorf("%w: mutable endpoints", ErrCorrupt)
	}
	if state.head.count < 0 || state.head.count > s.opts.MaxHeadSamples || size > s.opts.MaxHeadBytes {
		return state, fmt.Errorf("%w: mutable head capacity", ErrLimit)
	}
	if size == 0 || len(state.head.packed) != size {
		return state, fmt.Errorf("%w: mutable body missing", ErrCorrupt)
	}
	state.head.start, state.head.end = first.Int64, last.Int64
	return state, nil
}

// ingestUpdate is one UPDATE for one series' write. It moves max_seen_ts once for
// the whole batch, and names ready and next_gc_ts only when they change: both are
// indexed, and naming a column rewrites its index entry even for the same value.
func ingestUpdate(write headWrite, next headUpdate) (string, []any) {
	state := write.state
	query := `update series_state set tail=?, head_count=?, head_start=?, head_end=?, max_seen_ts=?, version=version+1`
	arguments := []any{next.packed, next.count, next.first, next.last, write.newest()}

	if ready := boolInt(next.ready); ready != state.ready {
		query += `, ready=?`
		arguments = append(arguments, ready)
	}
	if due := earlierExpiry(state.nextGC, write.incoming[0].At); due != state.nextGC {
		query += `, next_gc_ts=?`
		arguments = append(arguments, due)
	}

	query += ` where series_id=?`
	return query, append(arguments, write.id)
}

// earlierExpiry: the oldest sample in a head decides when expiry is next due.
func earlierExpiry(due sql.NullInt64, at int64) sql.NullInt64 {
	if due.Valid && due.Int64 <= at {
		return due
	}
	return sql.NullInt64{Int64: at, Valid: true}
}

func boolInt(value bool) int {
	if value {
		return 1
	}
	return 0
}
