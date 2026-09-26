package main

import (
	"context"
	"database/sql"
	"errors"
	"time"

	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/sqldb"
)

// a session's term, and the age of its last renewal past which a read renews
// it: a thirtieth of the term, the RefreshEvery of kv.Sliding
const (
	sessionTerm  = 30 * 24 * time.Hour
	sessionRenew = sessionTerm / 30
)

// session is what a signed-in device keeps
type session struct {
	Device string `json:"device"`
	Since  int64  `json:"since"`
}

// storedSession is a session signed in before the phase began
type storedSession struct {
	user    int64
	token   string
	session session
	expires time.Time
}

// sessions is the sessions case on one implementation: the cookie carries the
// user's id and a token
type sessions interface {
	preload(ctx context.Context, batch []storedSession) error
	signIn(ctx context.Context, user int64, token string, s session) error
	// read says whether the session is live, and whether reading it renewed it
	read(ctx context.Context, user int64, token string) (found, renewed bool, err error)
	signOut(ctx context.Context, user int64, token string) error
	signOutEverywhere(ctx context.Context, user int64) error
	devices(ctx context.Context, user int64) (int, error)
}

func openSessions(ctx context.Context, b *backend) (sessions, error) {
	if b.impl == implKV {
		return openKVSessions(ctx, b)
	}
	b.sweep("sessions", sweepSessions)
	return &tableSessions{app: b.app, now: b.now}, nil
}

// stale says that a session whose expiry is short of the term by more than a
// thirtieth was last renewed long enough ago to be renewed again
func stale(now, expires time.Time) bool {
	return expires.Before(now.Add(sessionTerm - sessionRenew))
}

// tableSessions is the sessions table an application keeps without kv
type tableSessions struct {
	app *sqldb.DB
	now func() time.Time
}

const (
	upsertSession = `insert into sessions (user_id, token, device, since, expires_at) values (?1, ?2, ?3, ?4, ?5)
		on conflict (user_id, token) do update set
			device = excluded.device, since = excluded.since, expires_at = excluded.expires_at`
	selectSession = `select device, since, expires_at from sessions
		where user_id = ?1 and token = ?2 and expires_at > ?3`
	renewSession  = `update sessions set expires_at = ?3 where user_id = ?1 and token = ?2 and expires_at > ?4`
	deleteSession = `delete from sessions where user_id = ?1 and token = ?2`
	deleteUser    = `delete from sessions where user_id = ?1`
	listDevices   = `select device from sessions where user_id = ?1 and expires_at > ?2 order by token limit 20`
	sweepSessions = `delete from sessions where expires_at <= ?1`
	countSessions = `select count(*) from sessions`
)

type sessionRow struct {
	Device    string
	Since     int64
	ExpiresAt int64
}

func (t *tableSessions) preload(ctx context.Context, batch []storedSession) error {
	return t.app.Tx(ctx, func(tx *sqldb.Tx) error {
		for _, s := range batch {
			_, err := tx.Exec(ctx, upsertSession, s.user, s.token, s.session.Device, s.session.Since,
				s.expires.UnixMilli())
			if err != nil {
				return err
			}
		}
		return nil
	})
}

func (t *tableSessions) signIn(ctx context.Context, user int64, token string, s session) error {
	_, err := t.app.Exec(ctx, upsertSession, user, token, s.Device, s.Since, t.now().Add(sessionTerm).UnixMilli())
	return err
}

// read renews a stale session inside the request, a transaction on the writer,
// as a careful handler without kv.Sliding does
func (t *tableSessions) read(ctx context.Context, user int64, token string) (found, renewed bool, err error) {
	now := t.now()
	row, err := sqldb.One[sessionRow](ctx, t.app, selectSession, user, token, now.UnixMilli())
	switch {
	case errors.Is(err, sql.ErrNoRows):
		return false, false, nil
	case err != nil || !stale(now, time.UnixMilli(row.ExpiresAt)):
		return err == nil, false, err
	}
	result, err := t.app.Exec(ctx, renewSession, user, token, now.Add(sessionTerm).UnixMilli(), now.UnixMilli())
	err = conflictUnless(result, err)
	if errors.Is(err, errMoved) {
		return true, false, nil
	}
	return true, err == nil, err
}

func (t *tableSessions) signOut(ctx context.Context, user int64, token string) error {
	_, err := t.app.Exec(ctx, deleteSession, user, token)
	return err
}

func (t *tableSessions) signOutEverywhere(ctx context.Context, user int64) error {
	_, err := t.app.Exec(ctx, deleteUser, user)
	return err
}

func (t *tableSessions) devices(ctx context.Context, user int64) (int, error) {
	devices, err := sqldb.All[string](ctx, t.app, listDevices, user, t.now().UnixMilli())
	return len(devices), err
}

// kvSessions is the sessions case through kv as it is built: a stale session
// renewed by Touch inside the request, as the table renews it, and a sign-out
// everywhere deleting what a Scan finds. kv.Sliding(sessionTerm) takes the
// place of DefaultTTL and of read's Touch, and Clear of signOutEverywhere's loop
type kvSessions struct {
	state  *kv.Store
	bucket *kv.Bucket[session]
	now    func() time.Time
}

func openKVSessions(ctx context.Context, b *backend) (sessions, error) {
	bucket, err := kv.OpenBucket[session](ctx, b.state, "sessions", kv.DefaultTTL(sessionTerm))
	if err != nil {
		return nil, err
	}
	return &kvSessions{state: b.state, bucket: bucket, now: b.now}, nil
}

func (k *kvSessions) preload(ctx context.Context, batch []storedSession) error {
	return k.state.Tx(ctx, func(tx *kv.Tx) error {
		inTx := k.bucket.WithTx(tx)
		for _, s := range batch {
			if err := inTx.Of(s.user).Set(ctx, s.token, s.session, kv.ExpireAt(s.expires)); err != nil {
				return err
			}
		}
		return nil
	})
}

func (k *kvSessions) signIn(ctx context.Context, user int64, token string, s session) error {
	return k.bucket.Of(user).Set(ctx, token, s)
}

func (k *kvSessions) read(ctx context.Context, user int64, token string) (found, renewed bool, err error) {
	mine := k.bucket.Of(user)
	entry, found, err := mine.GetEntry(ctx, token)
	if err != nil || !found || !stale(k.now(), entry.ExpiresAt) {
		return found, false, err
	}
	renewed, err = mine.Touch(ctx, token)
	return true, renewed, err
}

func (k *kvSessions) signOut(ctx context.Context, user int64, token string) error {
	return k.bucket.Of(user).Delete(ctx, token)
}

// signOutEverywhere deletes, in one transaction, every session a Scan of the
// user's branch finds, a page at a time
func (k *kvSessions) signOutEverywhere(ctx context.Context, user int64) error {
	return k.state.Tx(ctx, func(tx *kv.Tx) error {
		mine := k.bucket.WithTx(tx).Of(user)
		for more := true; more; {
			page, err := mine.Scan(ctx, kv.Query{Limit: 1000})
			if err != nil {
				return err
			}
			for _, entry := range page.Entries {
				if err = mine.Delete(ctx, entry.Key); err != nil {
					return err
				}
			}
			more = page.More
		}
		return nil
	})
}

func (k *kvSessions) devices(ctx context.Context, user int64) (int, error) {
	page, err := k.bucket.Of(user).Scan(ctx, kv.Query{Limit: 20})
	return len(page.Entries), err
}
