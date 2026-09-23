package metrics

import (
	"context"
	"database/sql"
	"fmt"
	"math"
	"sort"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

type preparedBatch struct {
	identity string
	labels   []Label
	kind     Kind
	samples  []Sample
}

type ingestState struct {
	frontier, nextGC, failedAt sql.NullInt64
	failure                    sql.NullString
	version, maxSeen           int64
	ready                      int
	head                       headSnapshot
	legacy                     bool
}

// Ingest commits all batches together; duplicate mutable timestamps keep the last supplied value.
func (s *Store) Ingest(ctx context.Context, batches []Batch) (err error) {
	defer func() {
		if err != nil {
			s.rejected.Add(1)
		}
	}()
	if err = s.enter(ctx); err != nil {
		return err
	}
	defer s.leave()
	select {
	case s.ingestSlots <- struct{}{}:
	case <-ctx.Done():
		return ctx.Err()
	}
	defer func() { <-s.ingestSlots }()
	if err = ctx.Err(); err != nil {
		return err
	}
	if len(batches) == 0 {
		return nil
	}
	if s.opts.SharedBudget != nil {
		weight, reserveErr := s.ingestReservation(batches)
		if reserveErr != nil {
			return reserveErr
		}
		if reserveErr := s.opts.SharedBudget.acquire(ctx, weight); reserveErr != nil {
			return reserveErr
		}
		defer s.opts.SharedBudget.release(weight)
	}
	cutoff := s.cutoff()
	prepared, err := s.prepareIngest(batches, cutoff)
	if err != nil {
		return err
	}
	if len(prepared) == 0 {
		return nil
	}
	written := 0
	err = s.file.UpdatePrepared(ctx, func(tx sqlite.Writer) error {
		for _, batch := range prepared {
			id, resolveErr := s.resolveSeries(ctx, tx, batch)
			if resolveErr != nil {
				return resolveErr
			}
			if writeErr := s.writeHead(ctx, tx, id, batch.samples, cutoff); writeErr != nil {
				return writeErr
			}
			written += len(batch.samples)
		}
		return nil
	})
	if err != nil {
		return fmt.Errorf("ingest metrics: %w", err)
	}
	s.ingested.Add(uint64(written)) //nolint:gosec // written counts admitted samples
	return nil
}

func (s *Store) prepareIngest(batches []Batch, cutoff int64) ([]preparedBatch, error) {
	type pending struct {
		batch  preparedBatch
		points map[int64]Sample
	}
	byIdentity := map[string]*pending{}
	inputSamples, inputBytes := 0, 0
	for _, batch := range batches {
		if len(batch.Samples) == 0 {
			return nil, fmt.Errorf("%w: empty series batch", ErrInvalid)
		}
		if len(batch.Samples) > s.opts.MaxBatchSamples-inputSamples {
			return nil, fmt.Errorf("%w: batch samples", ErrLimit)
		}
		inputSamples += len(batch.Samples)
		labels, identity, err := canonicalLabels(batch.Series.Labels, true)
		if err != nil {
			return nil, err
		}
		kind := batch.Series.Kind
		if kind == "" {
			kind = Gauge
		}
		if kind != Gauge && kind != Counter {
			return nil, fmt.Errorf("%w: series kind", ErrInvalid)
		}
		cost := len(identity) + 16*len(batch.Samples)
		if cost > s.opts.MaxBatchBytes-inputBytes {
			return nil, fmt.Errorf("%w: batch bytes", ErrLimit)
		}
		inputBytes += cost
		entry := byIdentity[identity]
		if entry == nil {
			entry = &pending{batch: preparedBatch{identity: identity, labels: labels, kind: kind}}
			byIdentity[identity] = entry
		}
		if entry.batch.kind != kind {
			return nil, fmt.Errorf("%w: conflicting kinds in batch", ErrInvalid)
		}
		last := int64(math.MinInt64)
		if len(entry.batch.samples) > 0 {
			last = entry.batch.samples[len(entry.batch.samples)-1].At
		}
		for _, point := range batch.Samples {
			if point.At == math.MaxInt64 {
				return nil, fmt.Errorf("%w: MaxInt64 is reserved for the exclusive range bound", ErrInvalid)
			}
			if point.At < cutoff {
				return nil, fmt.Errorf("%w: retention cutoff", ErrTooOld)
			}
			if entry.points == nil && point.At <= last {
				entry.points = make(map[int64]Sample, len(entry.batch.samples)+len(batch.Samples))
				for _, earlier := range entry.batch.samples {
					entry.points[earlier.At] = earlier
				}
				entry.batch.samples = nil
			}
			if entry.points != nil {
				entry.points[point.At] = point
			} else {
				entry.batch.samples = append(entry.batch.samples, point)
				last = point.At
			}
		}
	}
	prepared := make([]preparedBatch, 0, len(byIdentity))
	for _, entry := range byIdentity {
		if entry.points != nil {
			for _, point := range entry.points {
				entry.batch.samples = append(entry.batch.samples, point)
			}
			sort.Slice(entry.batch.samples, func(i, j int) bool { return entry.batch.samples[i].At < entry.batch.samples[j].At })
		}
		prepared = append(prepared, entry.batch)
	}
	sort.Slice(prepared, func(i, j int) bool { return prepared[i].identity < prepared[j].identity })
	return prepared, nil
}

func (s *Store) writeHead(ctx context.Context, tx sqlite.Writer, id int64, points []Sample, cutoff int64) error {
	state, err := s.loadIngestState(ctx, tx, id)
	if err != nil {
		return err
	}
	if state.failedAt.Valid {
		return fmt.Errorf("%w: %s", ErrSuspended, state.failure.String)
	}
	if state.frontier.Valid && points[0].At < state.frontier.Int64 {
		return fmt.Errorf("%w: sealed frontier", ErrTooOld)
	}
	if state.version == math.MaxInt64 {
		return fmt.Errorf("%w: series version exhausted", ErrLimit)
	}
	if state.head.count >= blockSamples && !state.legacy && s.opts.Lateness == 0 && cutoff <= state.head.start && points[0].At > state.head.end {
		packed, count, first, last, ready, appendErr := s.appendPackedHead(ctx, state, points, cutoff, id)
		if appendErr != nil {
			return appendErr
		}
		query, arguments := s.ingestUpdateParts(state, packed, count, first, last, ready, points, id)
		if _, updateErr := tx.ExecContext(ctx, query, arguments...); updateErr != nil { //nolint:gosec // query uses fixed fragments and binds every value
			return fmt.Errorf("replace mutable ingest state: %w", updateErr)
		}
		return nil
	}
	var existing []Sample
	if state.head.count > 0 {
		if state.legacy {
			existing, err = s.mutablePoints(ctx, tx, id)
		} else {
			existing, err = s.decodeHead(ctx, state.head)
		}
		if err != nil {
			return err
		}
	}
	merged, err := mergeHead(existing, points, s.opts.MaxHeadSamples)
	if err != nil {
		return err
	}
	prefix, prefixCount, err := reusableHeadPrefix(state.head.packed, points[0].At, s.opts.MaxHeadSamples)
	if err != nil {
		return err
	}
	packed, err := s.encodeHeadPrefix(ctx, id, merged, prefix, prefixCount)
	if err != nil {
		return err
	}
	query, arguments := s.ingestUpdate(state, packed, merged, points, id, cutoff)
	_, err = tx.ExecContext(ctx, query, arguments...) //nolint:gosec // query uses fixed fragments and binds every value
	if err != nil {
		return fmt.Errorf("replace mutable ingest state: %w", err)
	}
	if state.legacy {
		if _, err = tx.ExecContext(ctx, `delete from head where series_id=?`, id); err != nil {
			return fmt.Errorf("retire legacy head: %w", err)
		}
	}
	return nil
}

func (s *Store) ingestUpdate(state ingestState, packed []byte, merged, incoming []Sample, id, cutoff int64) (string, []any) {
	ready := 0
	maxSeen := max(state.maxSeen, incoming[len(incoming)-1].At)
	if s.headReady(merged, maxSeen, cutoff) {
		ready = 1
	}
	return s.ingestUpdateParts(state, packed, len(merged), merged[0].At, merged[len(merged)-1].At, ready, incoming, id)
}

func (s *Store) ingestUpdateParts(state ingestState, packed []byte, count int, firstAt, lastAt int64, ready int, incoming []Sample, id int64) (string, []any) {
	first := sql.NullInt64{Int64: firstAt, Valid: true}
	last := sql.NullInt64{Int64: lastAt, Valid: true}
	maxSeen := max(state.maxSeen, incoming[len(incoming)-1].At)
	nextGC := state.nextGC
	if !nextGC.Valid || incoming[0].At < nextGC.Int64 {
		nextGC = sql.NullInt64{Int64: incoming[0].At, Valid: true}
	}
	query := `update series_state set tail=?,head_count=?,head_start=?,head_end=?,max_seen_ts=?,version=version+1`
	arguments := []any{packed, count, first, last, maxSeen}
	if ready != state.ready {
		query += `,ready=?`
		arguments = append(arguments, ready)
	}
	if nextGC != state.nextGC {
		query += `,next_gc_ts=?`
		arguments = append(arguments, nextGC)
	}
	query += ` where series_id=?`
	arguments = append(arguments, id)
	return query, arguments
}

func (s *Store) loadIngestState(ctx context.Context, tx sqlite.Writer, id int64) (ingestState, error) {
	state := ingestState{head: headSnapshot{seriesID: id}}
	var first, last sql.NullInt64
	var size int
	err := sqlite.QueryRow(ctx, tx, `select sealed_before,version,max_seen_ts,ready,next_gc_ts,failed_at,failure_reason,head_count,head_start,head_end,coalesce(length(tail),0),case when length(tail)<=? then tail else null end from series_state where series_id=?`, s.opts.MaxHeadBytes, id).Scan(
		&state.frontier, &state.version, &state.maxSeen, &state.ready, &state.nextGC, &state.failedAt, &state.failure,
		&state.head.count, &first, &last, &size, &state.head.packed,
	)
	if err != nil {
		return state, fmt.Errorf("read ingest state: %w", err)
	}
	if state.failedAt.Valid {
		return state, nil
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
	state.head.start, state.head.end = first.Int64, last.Int64
	state.legacy = size == 0
	if !state.legacy && len(state.head.packed) != size {
		return state, fmt.Errorf("%w: mutable body missing", ErrCorrupt)
	}
	return state, nil
}
