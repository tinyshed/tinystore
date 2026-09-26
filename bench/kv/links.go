package main

import (
	"context"
	"database/sql"
	"errors"
	"time"

	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/sqldb"
)

// how long a sign-in code works
const codeTerm = 15 * time.Minute

// storedCode is a code asked for before the phase began
type storedCode struct {
	digest  []byte
	user    int64
	expires time.Time
}

// signInCodes is the sign-in link case on one implementation: one-time codes
// kept by their digest, so that a copy of the file holds no working link
type signInCodes interface {
	preload(ctx context.Context, batch []storedCode) error
	issue(ctx context.Context, digest []byte, user int64) error
	// take is the user a code signs in; a second take of it finds nothing
	take(ctx context.Context, digest []byte) (user int64, found bool, err error)
}

func openCodes(ctx context.Context, b *backend) (signInCodes, error) {
	if b.impl == implKV {
		codes, err := kv.OpenBucket[int64](ctx, b.state, "login-codes", kv.DefaultTTL(codeTerm))
		if err != nil {
			return nil, err
		}
		return &kvCodes{state: b.state, codes: codes}, nil
	}
	b.sweep("login_codes", sweepCodes)
	return &tableCodes{app: b.app, now: b.now}, nil
}

// tableCodes is the codes table an application keeps without kv
type tableCodes struct {
	app *sqldb.DB
	now func() time.Time
}

const (
	upsertCode = `insert into login_codes (digest, user_id, expires_at) values (?1, ?2, ?3)
		on conflict (digest) do update set user_id = excluded.user_id, expires_at = excluded.expires_at`
	takeCode   = `delete from login_codes where digest = ?1 and expires_at > ?2 returning user_id`
	sweepCodes = `delete from login_codes where expires_at <= ?1`
	countCodes = `select count(*) from login_codes`
)

func (t *tableCodes) preload(ctx context.Context, batch []storedCode) error {
	return t.app.Tx(ctx, func(tx *sqldb.Tx) error {
		for _, code := range batch {
			if _, err := tx.Exec(ctx, upsertCode, code.digest, code.user, code.expires.UnixMilli()); err != nil {
				return err
			}
		}
		return nil
	})
}

func (t *tableCodes) issue(ctx context.Context, digest []byte, user int64) error {
	_, err := t.app.Exec(ctx, upsertCode, digest, user, t.now().Add(codeTerm).UnixMilli())
	return err
}

// take reads and burns a code in one statement, so that of two clicks one signs in
func (t *tableCodes) take(ctx context.Context, digest []byte) (int64, bool, error) {
	user, err := sqldb.ExecScalar[int64](ctx, t.app, takeCode, digest, t.now().UnixMilli())
	if errors.Is(err, sql.ErrNoRows) {
		return 0, false, nil
	}
	return user, err == nil, err
}

// kvCodes is the sign-in link case through kv
type kvCodes struct {
	state *kv.Store
	codes *kv.Bucket[int64]
}

func (k *kvCodes) preload(ctx context.Context, batch []storedCode) error {
	return k.state.Tx(ctx, func(tx *kv.Tx) error {
		inTx := k.codes.WithTx(tx)
		for _, code := range batch {
			if err := inTx.Set(ctx, code.digest, code.user, kv.ExpireAt(code.expires)); err != nil {
				return err
			}
		}
		return nil
	})
}

func (k *kvCodes) issue(ctx context.Context, digest []byte, user int64) error {
	return k.codes.Set(ctx, digest, user)
}

func (k *kvCodes) take(ctx context.Context, digest []byte) (int64, bool, error) {
	return k.codes.Take(ctx, digest)
}
