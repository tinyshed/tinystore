package metrics

import (
	"context"
	"database/sql"
	"fmt"
)

// Maintain performs a bounded retention pass, then seals eligible full microblocks.
func (s *Store) Maintain(ctx context.Context) (Maintenance, error) {
	if err := s.enter(ctx); err != nil {
		return Maintenance{}, err
	}
	defer s.leave()

	release, err := s.holdMaintenance(ctx)
	if err != nil {
		return Maintenance{}, err
	}
	defer release()

	unreserve, err := s.reserve(ctx, s.maintenanceReservation)
	if err != nil {
		return Maintenance{}, err
	}
	defer unreserve()

	pass := maintenancePass{store: s, cutoff: s.cutoff()}
	if err = pass.expireDue(ctx); err != nil {
		return pass.result, err
	}

	err = pass.sealReady(ctx)
	return pass.result, err
}

// holdMaintenance lets one maintenance call at a time run on a store.
func (s *Store) holdMaintenance(ctx context.Context) (release func(), err error) {
	select {
	case <-s.maintenanceGate:
		return func() { s.maintenanceGate <- struct{}{} }, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

// maintenancePass is one Maintain call: its cutoff, the publications it has
// staged but not yet written, and what it has done so far.
type maintenancePass struct {
	store       *Store
	cutoff      int64
	staged      []stagedPublication
	stagedBytes int
	result      Maintenance
}

func (p *maintenancePass) expireDue(ctx context.Context) error {
	due, err := p.store.expiryDue(ctx, p.cutoff)
	if err != nil {
		return err
	}
	for _, id := range due {
		expired, reclaimed, expireErr := p.store.expireSeries(ctx, id, p.cutoff)
		if expireErr != nil {
			if err = p.isolate(ctx, id, "retention", expireErr); err != nil {
				return err
			}
			continue
		}
		p.result.ExpiredSamples += expired
		p.store.expired.Add(uint64(expired)) //nolint:gosec // expiry only counts removed samples
		if reclaimed {
			p.result.ReclaimedSeries++
			p.store.reclaimed.Add(1)
		}
	}
	return nil
}

func (p *maintenancePass) sealReady(ctx context.Context) error {
	ready, err := p.store.readyToSeal(ctx)
	if err != nil {
		return err
	}
	for _, id := range ready {
		if err = p.seal(ctx, id); err != nil {
			return err
		}
	}
	return p.flush(ctx)
}

// seal encodes the safe prefix of one series outside the writer and stages it
// for publication.
func (p *maintenancePass) seal(ctx context.Context, id int64) error {
	candidate, err := p.store.readCandidate(ctx, id, p.cutoff)
	if err != nil {
		return p.isolate(ctx, id, "read head", err)
	}
	if len(candidate.points) < blockSamples {
		return p.store.clearReady(ctx, candidate)
	}

	group, err := p.store.encodeCandidate(ctx, candidate)
	if err != nil {
		return p.isolate(ctx, id, "encode block", err)
	}
	consumed := 0
	for _, block := range group.blocks {
		consumed += block.head.Count
	}
	candidate.points = candidate.points[:consumed]

	return p.stage(ctx, stagedPublication{candidate: candidate, group: group})
}

// isolate suspends a series whose maintenance failed on its own data or
// limits, so that the pass goes on; any other failure ends the pass.
func (p *maintenancePass) isolate(ctx context.Context, id int64, phase string, cause error) error {
	quarantined, err := p.store.handleMaintenanceFailure(ctx, id, phase, cause)
	if err != nil {
		return err
	}
	if quarantined {
		p.result.QuarantinedSeries++
	}
	return nil
}

// stage writes the batch first when this publication would not fit in it, and
// after when the batch is full.
func (p *maintenancePass) stage(ctx context.Context, publication stagedPublication) error {
	bytes := publicationBytes(publication.candidate, publication.group)
	full := len(p.staged) == publicationBatchSeries || p.stagedBytes+bytes > publicationBatchBytes
	if len(p.staged) > 0 && full {
		if err := p.flush(ctx); err != nil {
			return err
		}
	}

	p.staged = append(p.staged, publication)
	p.stagedBytes += bytes
	if len(p.staged) == publicationBatchSeries || p.stagedBytes >= publicationBatchBytes {
		return p.flush(ctx)
	}
	return nil
}

func (p *maintenancePass) flush(ctx context.Context) error {
	committed, err := p.store.publishBatch(ctx, p.staged, p.cutoff)
	if err != nil {
		return err
	}
	p.result.SealedBlocks += committed.SealedBlocks
	p.result.Conflicts += committed.Conflicts
	p.result.QuarantinedSeries += committed.QuarantinedSeries
	p.store.sealed.Add(uint64(committed.SealedBlocks))          //nolint:gosec // only committed blocks are counted
	p.store.quarantined.Add(int64(committed.QuarantinedSeries)) //nolint:gosec // bounded by staged series count
	p.staged = nil
	p.stagedBytes = 0
	return nil
}

const expiryDueQuery = `
	select series_id from series_state
	where failed_at is null and next_gc_ts is not null and next_gc_ts<?
	order by next_gc_ts,series_id limit cast(? as integer)`

// expiryDue finds the series whose oldest sample is behind the cutoff through
// one indexed value per series.
func (s *Store) expiryDue(ctx context.Context, cutoff int64) ([]int64, error) {
	ctx, cancel := context.WithTimeout(ctx, s.opts.SnapshotTimeout)
	defer cancel()
	var ids []int64
	err := s.file.View(ctx, func(tx *sql.Tx) error {
		var err error
		ids, err = readSeriesIDs(ctx, tx, expiryDueQuery, cutoff, s.opts.MaintenanceSeries)
		return err
	})
	return ids, err
}

const (
	readyAfterQuery = `
		select series_id from series_state
		where failed_at is null and ready=1 and series_id>?
		order by series_id limit cast(? as integer)`
	readyUpToQuery = `
		select series_id from series_state
		where failed_at is null and ready=1 and series_id<=?
		order by series_id limit cast(? as integer)`
)

// readyToSeal continues after the last series the previous pass took, and wraps
// around, so that the series early in the table cannot starve the rest.
func (s *Store) readyToSeal(ctx context.Context) ([]int64, error) {
	ctx, cancel := context.WithTimeout(ctx, s.opts.SnapshotTimeout)
	defer cancel()
	var ids []int64
	err := s.file.View(ctx, func(tx *sql.Tx) error {
		cursor := s.readyCursor.Load()
		var err error
		if ids, err = readSeriesIDs(ctx, tx, readyAfterQuery, cursor, s.opts.MaintenanceSeries); err != nil {
			return err
		}
		if len(ids) < s.opts.MaintenanceSeries && cursor > 0 {
			wrapped, err := readSeriesIDs(ctx, tx, readyUpToQuery, cursor, s.opts.MaintenanceSeries-len(ids))
			ids = append(ids, wrapped...)
			return err
		}
		return nil
	})
	if err == nil && len(ids) > 0 {
		s.readyCursor.Store(ids[len(ids)-1])
	}
	return ids, err
}

func readSeriesIDs(ctx context.Context, tx *sql.Tx, query string, arguments ...any) ([]int64, error) {
	rows, err := tx.QueryContext(ctx, query, arguments...)
	if err != nil {
		return nil, fmt.Errorf("find due series: %w", err)
	}
	defer rows.Close()
	var ids []int64
	for rows.Next() {
		var id int64
		if err = rows.Scan(&id); err != nil {
			return ids, fmt.Errorf("read due series: %w", err)
		}
		ids = append(ids, id)
	}
	if err = rows.Err(); err != nil {
		return ids, fmt.Errorf("iterate due series: %w", err)
	}
	return ids, nil
}
