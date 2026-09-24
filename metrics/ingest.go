package metrics

import (
	"context"
	"fmt"
	"math"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Ingest stores every batch or none of them. A timestamp repeated within the
// call, or already waiting in the head, keeps the value supplied last.
func (s *Store) Ingest(ctx context.Context, batches []Batch) (err error) {
	defer s.countRejection(&err)

	release, err := s.admit(ctx, s.ingestSlots)
	if err != nil {
		return err
	}
	defer release()

	if len(batches) == 0 {
		return nil
	}

	unreserve, err := s.reserve(ctx, func() (int64, error) { return s.ingestReservation(batches) })
	if err != nil {
		return err
	}
	defer unreserve()

	cutoff := s.cutoff()
	input, err := s.prepareIngest(batches, cutoff)
	if err != nil {
		return err
	}

	if err = s.commitIngest(ctx, input, cutoff); err != nil {
		return fmt.Errorf("ingest metrics: %w", err)
	}
	s.ingested.Add(uint64(countSamples(input))) //nolint:gosec // a count of committed samples
	return nil
}

// commitIngest registers new series and rewrites every touched head in one
// transaction, so a failure anywhere leaves the file as it was.
func (s *Store) commitIngest(ctx context.Context, input []preparedBatch, cutoff int64) error {
	return s.file.UpdatePrepared(ctx, func(tx sqlite.Writer) error {
		for _, series := range input {
			if err := s.writeSeries(ctx, tx, series, cutoff); err != nil {
				return seriesError(series.labels, err)
			}
		}
		return nil
	})
}

// writeSeries registers a series the first time it arrives, then writes its
// samples into its head.
func (s *Store) writeSeries(ctx context.Context, tx sqlite.Writer, series preparedBatch, cutoff int64) error {
	id, err := s.resolveSeries(ctx, tx, series)
	if err != nil {
		return err
	}
	return s.writeHead(ctx, tx, id, series.samples, cutoff)
}

// writeHead puts one series' new samples into its packed head.
func (s *Store) writeHead(ctx context.Context, tx sqlite.Writer, id int64, incoming []Sample, cutoff int64) error {
	state, err := s.loadIngestState(ctx, tx, id)
	if err != nil {
		return err
	}

	err = state.accepts(incoming)
	if err != nil {
		return err
	}

	chunks, err := s.storedChunks(state)
	if err != nil {
		return err
	}
	write := headWrite{id: id, state: state, chunks: chunks, incoming: incoming, cutoff: cutoff}

	next, err := s.nextHead(ctx, write)
	if err != nil {
		return err
	}

	query, arguments := ingestUpdate(write, next)
	_, err = tx.ExecContext(ctx, query, arguments...) //nolint:gosec // fixed fragments, every value bound
	if err != nil {
		return fmt.Errorf("replace mutable ingest state: %w", err)
	}
	return nil
}

// accepts refuses a write the series can no longer take:
//
//	suspended by maintenance          ErrSuspended
//	older than the sealed frontier    ErrTooOld
//	version counter exhausted         ErrLimit
func (state ingestState) accepts(incoming []Sample) error {
	switch {
	case state.failedAt.Valid:
		return fmt.Errorf("%w: %s", ErrSuspended, state.failure.String)
	case state.frontier.Valid && incoming[0].At < state.frontier.Int64:
		return fmt.Errorf("%w: sealed frontier", ErrTooOld)
	case state.version == math.MaxInt64:
		return fmt.Errorf("%w: series version exhausted", ErrLimit)
	}
	return nil
}

// headWrite is one series' write while its next head is being built.
type headWrite struct {
	id       int64
	state    ingestState
	chunks   []headChunk // the stored head, parsed
	incoming []Sample
	cutoff   int64
}

// newest is the series' newest timestamp once this write lands.
func (w headWrite) newest() int64 {
	return max(w.state.maxSeen, w.incoming[len(w.incoming)-1].At)
}

// headUpdate is a head after one write, ready to be stored.
type headUpdate struct {
	packed      []byte
	count       int
	first, last int64
	ready       bool // 240 retained samples lie strictly before the watermark
}

// storedChunks parses the head a write starts from; a new series has none.
func (s *Store) storedChunks(state ingestState) ([]headChunk, error) {
	if state.head.count == 0 {
		return nil, nil
	}
	return s.parseHead(state.head)
}

// nextHead builds the head after this write. A long head that only grows at its
// end keeps its full chunks; anything else is decoded and merged whole.
func (s *Store) nextHead(ctx context.Context, write headWrite) (headUpdate, error) {
	if s.onlyAppends(write) {
		return s.appendToHead(ctx, write)
	}
	return s.mergeIntoHead(ctx, write)
}

// onlyAppends: the head is long, nothing may arrive late, nothing in it has
// expired, and every incoming sample is newer than all of it.
//
//	head       [240][240][ 90]            ends at 14:00
//	incoming                   14:01 14:02
//	           kept      [90 + 2] encoded again
func (s *Store) onlyAppends(write headWrite) bool {
	head := write.state.head
	return head.count >= blockSamples &&
		s.opts.Lateness == 0 &&
		write.cutoff <= head.start &&
		write.incoming[0].At > head.end
}

// appendToHead decodes only what follows the chunks it keeps.
func (s *Store) appendToHead(ctx context.Context, write headWrite) (headUpdate, error) {
	kept := reusableChunks(write.chunks, write.incoming[0].At)
	keptSamples := countChunkSamples(kept)

	tail, err := s.decodeChunks(ctx, write.chunks[len(kept):], math.MinInt64, math.MaxInt64)
	if err != nil {
		return headUpdate{}, err
	}
	tail, err = mergeHead(tail, write.incoming, s.opts.MaxHeadSamples-keptSamples)
	if err != nil {
		return headUpdate{}, err
	}

	packed, err := s.encodeHeadAfter(ctx, write.id, kept, tail)
	if err != nil {
		return headUpdate{}, err
	}

	// kept samples are retained and older than the newest, so every one may seal
	sealable := keptSamples
	for _, point := range tail {
		if point.At >= write.cutoff && point.At < write.newest() {
			sealable++
		}
	}

	return headUpdate{
		packed: packed,
		count:  keptSamples + len(tail),
		first:  write.state.head.start,
		last:   tail[len(tail)-1].At,
		ready:  sealable >= blockSamples,
	}, nil
}

// mergeIntoHead decodes the whole head, merges, and encodes again what changed.
func (s *Store) mergeIntoHead(ctx context.Context, write headWrite) (headUpdate, error) {
	existing, err := s.decodeChunks(ctx, write.chunks, math.MinInt64, math.MaxInt64)
	if err != nil {
		return headUpdate{}, err
	}
	merged, err := mergeHead(existing, write.incoming, s.opts.MaxHeadSamples)
	if err != nil {
		return headUpdate{}, err
	}

	kept := reusableChunks(write.chunks, write.incoming[0].At)
	packed, err := s.encodeHeadAfter(ctx, write.id, kept, merged[countChunkSamples(kept):])
	if err != nil {
		return headUpdate{}, err
	}

	return headUpdate{
		packed: packed,
		count:  len(merged),
		first:  merged[0].At,
		last:   merged[len(merged)-1].At,
		ready:  s.headReady(merged, write.newest(), write.cutoff),
	}, nil
}

// countRejection counts a failed call, whichever step failed.
func (s *Store) countRejection(err *error) {
	if *err != nil {
		s.rejected.Add(1)
	}
}
