package kv

import (
	"context"
	"database/sql"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// the oldest expired keys first, through the expiry index, with the spilled
// values they name
const expireCells = `delete from cells where (bucket, path) in (
		select bucket, path from cells where expires <= ?1 order by expires limit cast(?2 as integer)
	) returning spill`

// Maintenance is what one Maintain call did.
type Maintenance struct {
	Expired int
}

// Maintain deletes expired keys and the values they spilled, 10,000 a
// transaction and at most ten transactions a call; the store calls it every
// minute unless it is Manual. No read returns an expired key, whether Maintain
// has deleted it or not.
func (s *Store) Maintain(ctx context.Context) (Maintenance, error) {
	release, err := s.holdMaintenance(ctx)
	if err != nil {
		return Maintenance{}, err
	}
	defer release()
	leave, err := s.admit(ctx)
	if err != nil {
		return Maintenance{}, err
	}
	defer leave()

	now := s.now().UnixMilli()
	var done Maintenance
	for range expiryBatches {
		expired, err := s.expireBatch(ctx, now)
		done.Expired += expired
		if err != nil || expired < expiryBatch {
			return done, err
		}
	}
	return done, nil
}

func (s *Store) maintainInBackground(ctx context.Context) error {
	_, err := s.Maintain(ctx)
	return err
}

// holdMaintenance lets one Maintain at a time run on a store
func (s *Store) holdMaintenance(ctx context.Context) (release func(), err error) {
	select {
	case <-s.maintenance:
		return func() { s.maintenance <- struct{}{} }, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

// expireBatch deletes one batch of expired keys in a transaction of its own
func (s *Store) expireBatch(ctx context.Context, now int64) (int, error) {
	var spilled []int64
	expired := 0
	err := s.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		spilled, expired = spilled[:0], 0
		rows, err := w.QueryContext(ctx, expireCells, now, expiryBatch) //nolint:rowserrcheck // EachRow checks Err
		if err != nil {
			return err
		}
		err = sqlite.EachRow(rows, "expired keys", func(rows *sql.Rows) error {
			var spill sql.NullInt64
			if scanErr := rows.Scan(&spill); scanErr != nil {
				return scanErr
			}
			expired++
			if spill.Valid {
				spilled = append(spilled, spill.Int64)
			}
			return nil
		})
		for _, id := range spilled {
			if err == nil {
				_, err = w.ExecContext(ctx, deleteSpilled, id)
			}
		}
		return err
	})
	return expired, err
}
