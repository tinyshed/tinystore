package jobs

import (
	"context"
	"database/sql"
	"errors"
	"log/slog"
	"sync"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Maintenance is what one Maintain call removed.
type Maintenance struct {
	Failed int // failed jobs past their queue's KeepFailed
	Done   int // keys past their queue's KeepDone
}

// the oldest failed jobs of a queue first, with the values they spilled, and
// the keys remembered longest ago
const (
	expireFailed = `delete from failed where (queue, id) in (
			select queue, id from failed where queue = ?1 and failed <= ?2 order by failed limit cast(?3 as integer)
		) returning spill`
	forgetDone = `delete from done where (queue, key) in (
			select queue, key from done where until <= ?1 order by until limit cast(?2 as integer)
		)`
)

// Maintain removes the failed jobs each queue this process opened keeps no
// longer, and the done keys past their KeepDone, 10,000 a transaction and at
// most ten transactions of each a call; the store calls it every minute unless
// it is Manual. A queue this process has not opened keeps its failed jobs.
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
	var errs []error
	now := s.clock()
	for _, state := range s.openQueues() {
		removed, expireErr := s.batches(ctx, func(w sqlite.Writer) (int, error) {
			return expireBatch(ctx, w, state.id, now-state.policy.keepFailed.Milliseconds())
		})
		done.Failed += removed
		errs = append(errs, expireErr)
	}
	removed, forgetErr := s.batches(ctx, func(w sqlite.Writer) (int, error) {
		result, err := w.ExecContext(ctx, forgetDone, now, maintainBatch)
		if err != nil {
			return 0, err
		}
		forgotten, err := result.RowsAffected()
		return int(forgotten), err
	})
	done.Done = removed
	return done, errors.Join(append(errs, forgetErr)...)
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

func (s *Store) openQueues() []*queueState {
	s.opened.Lock()
	defer s.opened.Unlock()
	states := make([]*queueState, 0, len(s.queues))
	for _, state := range s.queues {
		states = append(states, state)
	}
	return states
}

// batches runs one batch a transaction until a batch removes fewer than a
// full one, at most ten times
func (s *Store) batches(ctx context.Context, batch func(sqlite.Writer) (int, error)) (int, error) {
	total := 0
	for range 10 {
		removed := 0
		err := s.file.UpdatePrepared(ctx, func(w sqlite.Writer) (err error) {
			removed, err = batch(w)
			return err
		})
		total += removed
		if err != nil || removed < maintainBatch {
			return total, err
		}
	}
	return total, nil
}

// expireBatch removes one batch of a queue's failed jobs that failed before
// cutoff, and the values they spilled
func expireBatch(ctx context.Context, w sqlite.Writer, queue, cutoff int64) (int, error) {
	limit := maintainBatch
	rows, err := w.QueryContext(ctx, expireFailed, queue, cutoff, limit) //nolint:rowserrcheck // EachRow checks Err
	if err != nil {
		return 0, err
	}
	var spilled []sql.NullInt64
	err = sqlite.EachRow(rows, "expired failed jobs", func(rows *sql.Rows) error {
		var spill sql.NullInt64
		scanErr := rows.Scan(&spill)
		spilled = append(spilled, spill)
		return scanErr
	})
	for _, spill := range spilled {
		if err == nil {
			err = dropSpilled(ctx, w, spill)
		}
	}
	return len(spilled), err
}

// quietLog logs one kind of event of one queue once a quiet period, with how
// many happened since, so that a million failures are not a million lines:
//
//	00:00  a job failed for good   → Warn count=1
//	00:01…00:09 1,204 more         → counted
//	00:10  one more                → Warn count=1205
type quietLog struct {
	log      *slog.Logger
	message  string
	queue    string
	mu       sync.Mutex
	count    int
	loggedAt time.Time
}

func newQuietLog(log *slog.Logger, message, queue string) *quietLog {
	return &quietLog{log: log, message: message, queue: queue}
}

func (q *quietLog) observe(now time.Time, last string) {
	q.mu.Lock()
	defer q.mu.Unlock()
	q.count++
	if !q.loggedAt.IsZero() && now.Sub(q.loggedAt) < quietFailures {
		return
	}
	q.log.Warn(q.message, "queue", q.queue, "count", q.count, "last", last)
	q.count, q.loggedAt = 0, now
}
