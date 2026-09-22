package metrics

import (
	"context"
	"database/sql"
	"fmt"
	"math"
	"sort"
)

type preparedBatch struct {
	identity string
	labels   []Label
	kind     Kind
	samples  []Sample
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
	prepared, err := s.prepareIngest(batches, s.cutoff())
	if err != nil {
		return err
	}
	if len(prepared) == 0 {
		return nil
	}
	written := 0
	err = s.file.Update(ctx, func(tx *sql.Tx) error {
		for _, batch := range prepared {
			id, resolveErr := s.resolveSeries(ctx, tx, batch)
			if resolveErr != nil {
				return resolveErr
			}
			if writeErr := s.writeHead(ctx, tx, id, batch.samples); writeErr != nil {
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
			entry = &pending{batch: preparedBatch{identity: identity, labels: labels, kind: kind}, points: map[int64]Sample{}}
			byIdentity[identity] = entry
		}
		if entry.batch.kind != kind {
			return nil, fmt.Errorf("%w: conflicting kinds in batch", ErrInvalid)
		}
		for _, point := range batch.Samples {
			if point.At == math.MaxInt64 {
				return nil, fmt.Errorf("%w: MaxInt64 is reserved for the exclusive range bound", ErrInvalid)
			}
			if point.At < cutoff {
				return nil, fmt.Errorf("%w: retention cutoff", ErrTooOld)
			}
			entry.points[point.At] = point
		}
	}
	prepared := make([]preparedBatch, 0, len(byIdentity))
	for _, entry := range byIdentity {
		for _, point := range entry.points {
			entry.batch.samples = append(entry.batch.samples, point)
		}
		sort.Slice(entry.batch.samples, func(i, j int) bool { return entry.batch.samples[i].At < entry.batch.samples[j].At })
		prepared = append(prepared, entry.batch)
	}
	sort.Slice(prepared, func(i, j int) bool { return prepared[i].identity < prepared[j].identity })
	return prepared, nil
}

func (s *Store) writeHead(ctx context.Context, tx *sql.Tx, id int64, points []Sample) error {
	var frontier sql.NullInt64
	var version int64
	if err := tx.QueryRowContext(ctx, `select sealed_before,version from series_state where series_id=?`, id).Scan(&frontier, &version); err != nil {
		return fmt.Errorf("read ingest state: %w", err)
	}
	if frontier.Valid && points[0].At < frontier.Int64 {
		return fmt.Errorf("%w: sealed frontier", ErrTooOld)
	}
	if version == math.MaxInt64 {
		return fmt.Errorf("%w: series version exhausted", ErrLimit)
	}
	existing, err := s.mutablePoints(ctx, tx, id)
	if err != nil {
		return err
	}
	merged, err := mergeHead(existing, points, s.opts.MaxHeadSamples)
	if err != nil {
		return err
	}
	if err = s.saveHead(ctx, tx, id, merged); err != nil {
		return err
	}
	ready := 0
	if len(merged) >= blockSamples {
		ready = 1
	}
	_, err = tx.ExecContext(ctx, `update series_state set max_seen_ts=max(max_seen_ts,?),version=version+1,ready=?,next_gc_ts=case when next_gc_ts is null then ? else min(next_gc_ts,?) end where series_id=?`, points[len(points)-1].At, ready, points[0].At, points[0].At, id)
	if err != nil {
		return fmt.Errorf("advance ingest state: %w", err)
	}
	return nil
}
