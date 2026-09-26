package main

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/sqldb"
)

// a window of attempts from its first, and the attempts it lets through by
// address and by email
const (
	attemptWindow = 15 * time.Minute
	ipLimit       = 20
	emailLimit    = 5
)

// storedCount is a counter of attempts made before the phase began
type storedCount struct {
	scope, subject string
	attempts       int64
	expires        time.Time
}

// attempts is the attempt limits case on one implementation: counters by
// address and by email, each in a window fixed at its first attempt, which
// start again from zero once it ends
type attempts interface {
	preload(ctx context.Context, batch []storedCount) error
	// add counts n attempts and returns the window's count; past the int64 range it is ErrLimit
	add(ctx context.Context, scope, subject string, n int64) (int64, error)
	// attempt counts one attempt by address and one by email, as a sign-in does
	attempt(ctx context.Context, ip, email string) (byIP, byEmail int64, err error)
}

func openAttempts(ctx context.Context, b *backend) (attempts, error) {
	if b.impl == implKV {
		return openKVAttempts(ctx, b)
	}
	b.sweep("login_attempts", sweepAttempts)
	return &tableAttempts{app: b.app, now: b.now}, nil
}

// kvAttempts is the attempt limits case through kv's counters, kept in memory
// and written every second, since a crash may forget a second of attempts
type kvAttempts struct {
	state    *kv.Store
	counters *kv.Counters
}

func openKVAttempts(ctx context.Context, b *backend) (attempts, error) {
	counters, err := kv.OpenCounters(ctx, b.state, "login-attempts",
		kv.DefaultTTL(attemptWindow), kv.LoseAtMost(time.Second))
	if err != nil {
		return nil, err
	}
	return &kvAttempts{state: b.state, counters: counters}, nil
}

// preload adds the stored counts through memory and writes them; a counter
// takes no expiry of its own, so every prepared window ends one window from now
func (k *kvAttempts) preload(ctx context.Context, batch []storedCount) error {
	for _, c := range batch {
		if _, err := k.counters.Of(c.scope).Add(ctx, c.subject, c.attempts); err != nil {
			return err
		}
	}
	_, err := k.state.Maintain(ctx)
	return err
}

func (k *kvAttempts) add(ctx context.Context, scope, subject string, n int64) (int64, error) {
	return k.counters.Of(scope).Add(ctx, subject, n)
}

func (k *kvAttempts) attempt(ctx context.Context, ip, email string) (byIP, byEmail int64, err error) {
	if byIP, err = k.counters.Of("ip").Add(ctx, ip, 1); err != nil {
		return 0, 0, err
	}
	byEmail, err = k.counters.Of("email").Add(ctx, email, 1)
	return byIP, byEmail, err
}

// tableAttempts is the attempts table an application keeps without kv
type tableAttempts struct {
	app *sqldb.DB
	now func() time.Time
}

const (
	insertCount = `insert into login_attempts (scope, subject, attempts, expires_at) values (?1, ?2, ?3, ?4)`
	addAttempts = `insert into login_attempts (scope, subject, attempts, expires_at) values (?1, ?2, ?3, ?4)
		on conflict (scope, subject) do update set
			attempts = case when expires_at > ?5 then attempts + excluded.attempts else excluded.attempts end,
			expires_at = case when expires_at > ?5 then expires_at else excluded.expires_at end
			where expires_at <= ?5 or attempts <= 9223372036854775807 - excluded.attempts
		returning attempts`
	sweepAttempts = `delete from login_attempts where expires_at <= ?1`
	countAttempts = `select count(*) from login_attempts`
)

func (t *tableAttempts) preload(ctx context.Context, batch []storedCount) error {
	return t.app.Tx(ctx, func(tx *sqldb.Tx) error {
		for _, c := range batch {
			if _, err := tx.Exec(ctx, insertCount, c.scope, c.subject, c.attempts, c.expires.UnixMilli()); err != nil {
				return err
			}
		}
		return nil
	})
}

func (t *tableAttempts) add(ctx context.Context, scope, subject string, n int64) (int64, error) {
	return addIn(ctx, t.app, counted{scope: scope, subject: subject, n: n, now: t.now()})
}

// attempt counts both in one transaction, one commit a sign-in
func (t *tableAttempts) attempt(ctx context.Context, ip, email string) (byIP, byEmail int64, err error) {
	now := t.now()
	err = t.app.Tx(ctx, func(tx *sqldb.Tx) error {
		var addErr error
		if byIP, addErr = addIn(ctx, tx, counted{scope: "ip", subject: ip, n: 1, now: now}); addErr != nil {
			return addErr
		}
		byEmail, addErr = addIn(ctx, tx, counted{scope: "email", subject: email, n: 1, now: now})
		return addErr
	})
	return byIP, byEmail, err
}

// counted is n attempts at one counter, at the store's time
type counted struct {
	scope, subject string
	n              int64
	now            time.Time
}

// addIn counts attempts through h, the database or one of its transactions,
// and refuses a count past the int64 range rather than let SQLite make it a float
func addIn(ctx context.Context, h sqldb.Handle, c counted) (int64, error) {
	count, err := sqldb.ExecScalar[int64](ctx, h, addAttempts, c.scope, c.subject, c.n,
		c.now.Add(attemptWindow).UnixMilli(), c.now.UnixMilli())
	if errors.Is(err, sql.ErrNoRows) {
		return 0, fmt.Errorf("%w: attempts by %s %s past the int64 range", tinystore.ErrLimit, c.scope, c.subject)
	}
	return count, err
}
