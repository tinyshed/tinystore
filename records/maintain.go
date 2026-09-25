package records

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Maintain removes the segments and head rows retention has passed, then
// seals every head that holds a segment's worth or has waited
// Options.SealAge. A head that cannot be sealed does not keep the others
// waiting; its error is returned once the others are done.
func (s *Store) Maintain(ctx context.Context) (Maintenance, error) {
	release, err := s.admit(ctx)
	if err != nil {
		return Maintenance{}, err
	}
	defer release()

	hold, err := s.holdMaintenance(ctx)
	if err != nil {
		return Maintenance{}, err
	}
	defer hold()

	pass := maintenancePass{store: s, now: s.now()}
	if err = pass.expire(ctx); err != nil {
		return pass.result, err
	}

	err = pass.sealReady(ctx)
	return pass.result, err
}

// maintenancePass is one Maintain call: the time it runs at, and what it did
type maintenancePass struct {
	store  *Store
	now    time.Time
	result Maintenance
}

// cutoff is the oldest time retention keeps
func (p *maintenancePass) cutoff() int64 {
	return unixNanos(p.now.Add(-p.store.opts.Retention))
}

// sealBefore is when a head's oldest row must have been written for it to seal however small
func (p *maintenancePass) sealBefore() int64 {
	return unixNanos(p.now.Add(-p.store.opts.SealAge))
}

// headKey names one head: a stream's on-time records, or its late ones
type headKey struct {
	stream int64
	late   bool
}

const selectReadyHeads = `
	select stream, late from head_state
	where count >= ? or input >= ? or since <= ?
	order by since`

func (p *maintenancePass) sealReady(ctx context.Context) error {
	ready, err := p.store.readyHeads(ctx, p.sealBefore())
	if err != nil {
		return err
	}
	var failed error
	for _, head := range ready {
		err = p.sealHead(ctx, head)
		if errors.Is(err, tinystore.ErrCorrupt) {
			p.store.log.Error("head cannot be sealed", "stream", p.store.streams.name(head.stream), "error", err)
			failed = errors.Join(failed, err)
			continue
		}
		if err != nil {
			return err
		}
	}
	return failed
}

func (s *Store) readyHeads(ctx context.Context, sealBefore int64) ([]headKey, error) {
	var ready []headKey
	err := s.file.ViewPrepared(ctx, func(tx sqlite.Reader) error {
		//nolint:rowserrcheck // EachRow checks Err
		rows, err := tx.QueryContext(ctx, selectReadyHeads, maxSegmentRecords, maxSegmentInput, sealBefore)
		if err != nil {
			return err
		}
		return sqlite.EachRow(rows, "ready heads", func(rows *sql.Rows) error {
			var head headKey
			if err := rows.Scan(&head.stream, &head.late); err != nil {
				return err
			}
			ready = append(ready, head)
			return nil
		})
	})
	if err != nil {
		return nil, fmt.Errorf("records: find ready heads: %w", err)
	}
	return ready, nil
}
