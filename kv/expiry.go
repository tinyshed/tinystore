package kv

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"log/slog"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// the oldest expired keys first, through the expiry index, with the spilled
// values they name
const expireCells = `delete from _tinystore_kv_cells as cells where (bucket, path) in (
		select bucket, path from _tinystore_kv_cells as cells
		where expires <= ?1 order by expires limit cast(?2 as integer)
	) returning spill`

// Maintenance is what one Maintain call did.
type Maintenance struct {
	Flushed int // LoseAtMost counters written
	Renewed int // Sliding keys renewed
	Cleared int // rows a marked Clear hid, deleted
	Expired int
}

// Maintain writes what LoseAtMost counters hold and the renewals Sliding reads
// asked for, deletes the rows a marked Clear hid, then deletes expired keys and
// the values they spilled. It deletes expired keys 10,000 a transaction and at
// most ten transactions a call.
//
// The store calls it every minute unless it is Manual, and every ten seconds
// while the last call stopped at one of those bounds with rows left. It
// flushes counters and renewals on their own intervals.
//
// No read returns an expired or cleared key, whether Maintain has deleted it
// or not.
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

	var done Maintenance
	var errs [4]error
	var clearedLeft, expiredLeft bool
	done.Flushed, errs[0] = s.flushCounters(ctx)
	done.Renewed, errs[1] = s.flushRenewals(ctx)
	done.Cleared, clearedLeft, errs[2] = s.dropCleared(ctx)
	done.Expired, expiredLeft, errs[3] = s.expire(ctx)
	s.backlog.Store(clearedLeft || expiredLeft)
	return done, errors.Join(maintenanceError("flush counters", errs[0]), maintenanceError("renew keys", errs[1]),
		maintenanceError("clear branches", errs[2]), maintenanceError("expire keys", errs[3]))
}

func maintenanceError(phase string, err error) error {
	if err == nil {
		return nil
	}
	return fmt.Errorf("kv: %s: %w", phase, err)
}

// expire deletes expired keys a batch at a time, the oldest first. It says
// whether it stopped at its bound with more of them left.
func (s *Store) expire(ctx context.Context) (int, bool, error) {
	now := s.now().UnixMilli()
	total := 0
	for range expiryBatches {
		expired, err := s.expireBatch(ctx, now)
		total += expired
		if err != nil || expired < s.expireBound {
			return total, false, err
		}
	}
	return total, true, nil
}

// catchUpInBackground runs maintenance between its minutes while the last pass
// stopped at a bound with expired or cleared rows left.
//
// Without it a stream of expiring keys past 100,000 a minute would grow the
// file for good.
func (s *Store) catchUpInBackground(ctx context.Context) error {
	if !s.backlog.Load() {
		return nil
	}
	return s.maintainInBackground(ctx)
}

func (s *Store) maintainInBackground(ctx context.Context) error {
	started := time.Now()
	done, err := s.Maintain(ctx)
	if s.log.Enabled(ctx, slog.LevelDebug) {
		s.log.Debug("maintenance finished", "duration", time.Since(started), "flushed", done.Flushed,
			"renewed", done.Renewed, "cleared", done.Cleared, "expired", done.Expired, "failed", err != nil)
	}
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
		rows, err := w.QueryContext(ctx, expireCells, now, s.expireBound) //nolint:rowserrcheck // EachRow checks Err
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
