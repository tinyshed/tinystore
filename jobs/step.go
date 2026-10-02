package jobs

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"unicode/utf8"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// maxStepName bounds the name a handler gives a step
const maxStepName = 256

const (
	findStep = `select answer from _tinystore_jobs_steps where job = ?1 and name = ?2`
	// a step is kept only while the lease of the attempt keeping it holds the
	// job, so that one whose lease another claim took keeps nothing
	keepStep = `insert into _tinystore_jobs_steps (job, name, answer, at)
		select ?1, ?2, ?3, ?4 where exists (select 1 from _tinystore_jobs_leases where id = ?1 and attempt = ?5)
		on conflict (job, name) do update set answer = excluded.answer, at = excluded.at`
	dropSteps = `delete from _tinystore_jobs_steps where job = ?1`
)

// Step runs fn once for the job under name and keeps what it answered, as
// JSON: an attempt after a retry, a lost lease or a restart gets the kept
// answer back without running fn again.
//
//	hits, err := jobs.Step(ctx, job, "search", func(ctx context.Context) ([]Hit, error) {
//		return search(ctx, job.Value.Query)
//	})
//
// A step whose attempt ends before its answer is kept runs again, so what fn
// does outside the store should bear doing twice. A name is the step's within
// one run of the job, which a loop numbers: "model:1", "tool:1", "model:2". A
// run that ends, done, failed for good or cancelled, takes its steps along,
// and a repeating job's next run starts without them. An answer is at most
// 1 MiB of JSON.
func Step[T, V any](ctx context.Context, job Job[V], name string, fn func(context.Context) (T, error)) (T, error) {
	var answer T
	kept, found, err := job.Kept(ctx, name)
	if err != nil {
		return answer, err
	}
	if found {
		if err = json.Unmarshal(kept, &answer); err != nil {
			return answer, fmt.Errorf("%w: jobs: step %q kept an answer that no longer reads: %w", tinystore.ErrInvalid,
				name, err)
		}
		return answer, nil
	}
	if answer, err = fn(ctx); err != nil {
		return answer, err
	}
	encoded, err := json.Marshal(answer)
	if err != nil {
		return answer, fmt.Errorf("%w: jobs: step %q answered what JSON cannot write: %w", tinystore.ErrInvalid, name,
			err)
	}
	return answer, job.Keep(ctx, name, encoded)
}

// Kept is the answer the job's run kept under a step's name, which Step reads
// before it runs the step; found is false for a step not yet kept.
func (j Job[V]) Kept(ctx context.Context, name string) (answer json.RawMessage, found bool, err error) {
	l := j.lease
	if err = l.holding(name); err != nil {
		return nil, false, err
	}
	leave, err := l.store.admit(ctx)
	if err != nil {
		return nil, false, err
	}
	defer leave()
	reserved, err := l.store.reserve(ctx, maxValue)
	if err != nil {
		return nil, false, err
	}
	defer reserved.Release()
	err = l.store.file.ViewPrepared(ctx, func(r sqlite.Reader) error {
		return sqlite.QueryRowByKey(ctx, r, findStep, l.id, name).Scan(&answer)
	})
	if errors.Is(err, sql.ErrNoRows) {
		return nil, false, nil
	}
	if err != nil {
		return nil, false, nameFailure(l.queue.name, l.key, err)
	}
	return answer, true, nil
}

// Keep keeps answer, JSON, as what the job's run answered under a step's
// name, which Step does once its step has run: a later attempt of the run
// reads it with Kept. It returns once the answer is written, and refuses one
// whose lease another claim has taken.
func (j Job[V]) Keep(ctx context.Context, name string, answer json.RawMessage) error {
	l := j.lease
	if err := l.holding(name); err != nil {
		return err
	}
	switch {
	case !json.Valid(answer):
		return fmt.Errorf("%w: jobs: step %q's answer is not JSON", tinystore.ErrInvalid, name)
	case len(answer) > maxValue:
		return &tinystore.LimitError{Name: "bytes of a step's answer", Wanted: int64(len(answer)), Bound: maxValue}
	}
	reserved, err := l.store.reserve(ctx, len(answer))
	if err != nil {
		return err
	}
	defer reserved.Release()
	release, err := l.store.admitWrite(ctx)
	if err != nil {
		return err
	}
	defer release()
	err = l.store.file.UpdateGrouped(ctx, 0, func(w sqlite.Writer) error {
		result, execErr := w.ExecContext(ctx, keepStep, l.id, name, []byte(answer), l.store.clock(), l.attempt)
		if execErr != nil {
			return execErr
		}
		return changedOne(result)
	})
	if err != nil {
		return nameFailure(l.queue.name, l.key, err)
	}
	return nil
}

// holding refuses a step to a job no lease of this worker holds, and a name
// no step can have
func (l *lease) holding(name string) error {
	if l == nil {
		return errNotClaimed
	}
	if l.wasCancelled() {
		return fmt.Errorf("%w: %w", tinystore.ErrConflict, ErrCancelled)
	}
	if settled, lost := l.state(); lost {
		return errLeaseLost
	} else if settled {
		return errSettled
	}
	if name == "" || len(name) > maxStepName || !utf8.ValidString(name) {
		return fmt.Errorf("%w: jobs: a step's name is 1 to %d bytes of UTF-8, not %q", tinystore.ErrInvalid,
			maxStepName, name)
	}
	return nil
}

// runEnded drops the steps of a run that ended while its job stays, for a
// repeat or an Enqueue that asked for another run
func (l *lease) runEnded(ctx context.Context, w sqlite.Writer) error {
	_, err := w.ExecContext(ctx, dropSteps, l.id)
	return err
}
