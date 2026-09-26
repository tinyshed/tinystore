package main

import (
	"context"
	"database/sql"
	"errors"
	"time"

	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/sqldb"
)

// how long a claim holds an event, and how long a handled one is remembered
const (
	claimTerm   = 10 * time.Minute
	handledTerm = 7 * day
)

// storedEvent is an event handled before the phase began
type storedEvent struct {
	id      string
	expires time.Time
}

// webhookClaims is the webhook claims case on one implementation. A claim is
// its version as text, and "" when the event is handled or being handled;
// finish and release answer only to the event's live claim, to any other with
// a conflict, so that a handler whose claim expired cannot touch the next one's
type webhookClaims interface {
	preload(ctx context.Context, batch []storedEvent) error
	claim(ctx context.Context, event string) (string, error)
	// finish keeps a handled event for a week
	finish(ctx context.Context, event, claim string) error
	// release lets an event whose handler failed be delivered again
	release(ctx context.Context, event, claim string) error
}

func openClaims(ctx context.Context, b *backend) (webhookClaims, error) {
	if b.impl == implKV {
		seen, err := kv.OpenBucket[struct{}](ctx, b.state, "stripe-events")
		if err != nil {
			return nil, err
		}
		return &kvClaims{state: b.state, seen: seen}, nil
	}
	b.sweep("webhook_events", sweepEvents)
	return &tableClaims{app: b.app, now: b.now}, nil
}

// tableClaims is the events table an application keeps without kv
type tableClaims struct {
	app *sqldb.DB
	now func() time.Time
}

const (
	insertEvent = `insert into webhook_events (event_id, claim, expires_at) values (?1, ?2, ?3)`
	claimEvent  = `insert into webhook_events (event_id, claim, expires_at) values (?1, ?2, ?3)
		on conflict (event_id) do update set claim = excluded.claim, expires_at = excluded.expires_at
			where webhook_events.expires_at <= ?4
		returning claim`
	finishEvent  = `update webhook_events set expires_at = ?3 where event_id = ?1 and claim = ?2 and expires_at > ?4`
	releaseEvent = `delete from webhook_events where event_id = ?1 and claim = ?2 and expires_at > ?3`
	sweepEvents  = `delete from webhook_events where expires_at <= ?1`
	countEvents  = `select count(*) from webhook_events`
)

func (t *tableClaims) preload(ctx context.Context, batch []storedEvent) error {
	return t.app.Tx(ctx, func(tx *sqldb.Tx) error {
		for _, event := range batch {
			if _, err := tx.Exec(ctx, insertEvent, event.id, newEtag(), event.expires.UnixMilli()); err != nil {
				return err
			}
		}
		return nil
	})
}

// claim inserts the event, or takes over its row once its claim or its week
// has expired, in one statement
func (t *tableClaims) claim(ctx context.Context, event string) (string, error) {
	now := t.now()
	claim, err := sqldb.ExecScalar[int64](ctx, t.app, claimEvent, event, newEtag(), now.Add(claimTerm).UnixMilli(),
		now.UnixMilli())
	switch {
	case errors.Is(err, sql.ErrNoRows):
		return "", nil
	case err != nil:
		return "", err
	}
	return etagText(claim), nil
}

func (t *tableClaims) finish(ctx context.Context, event, claim string) error {
	etag, err := parseEtag(claim)
	if err != nil {
		return err
	}
	now := t.now()
	result, err := t.app.Exec(ctx, finishEvent, event, etag, now.Add(handledTerm).UnixMilli(), now.UnixMilli())
	return conflictUnless(result, err)
}

func (t *tableClaims) release(ctx context.Context, event, claim string) error {
	etag, err := parseEtag(claim)
	if err != nil {
		return err
	}
	result, err := t.app.Exec(ctx, releaseEvent, event, etag, t.now().UnixMilli())
	return conflictUnless(result, err)
}

// kvClaims is the webhook claims case through kv: a set of events, a claim
// being the version of an event's key
type kvClaims struct {
	state *kv.Store
	seen  *kv.Bucket[struct{}]
}

func (k *kvClaims) preload(ctx context.Context, batch []storedEvent) error {
	return k.state.Tx(ctx, func(tx *kv.Tx) error {
		inTx := k.seen.WithTx(tx)
		for _, event := range batch {
			if err := inTx.Set(ctx, event.id, struct{}{}, kv.ExpireAt(event.expires)); err != nil {
				return err
			}
		}
		return nil
	})
}

func (k *kvClaims) claim(ctx context.Context, event string) (string, error) {
	claim, first, err := k.seen.SetEntryIfAbsent(ctx, event, struct{}{}, kv.TTL(claimTerm))
	if err != nil || !first {
		return "", err
	}
	return claim.Version.String(), nil
}

func (k *kvClaims) finish(ctx context.Context, event, claim string) error {
	version, err := parseVersion(claim)
	if err != nil {
		return err
	}
	return k.seen.Set(ctx, event, struct{}{}, kv.IfVersion(version), kv.TTL(handledTerm))
}

func (k *kvClaims) release(ctx context.Context, event, claim string) error {
	version, err := parseVersion(claim)
	if err != nil {
		return err
	}
	return k.seen.Delete(ctx, event, kv.IfVersion(version))
}
