package main

import (
	"context"
	"database/sql"
	"errors"
	"time"

	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/sqldb"
)

// how long a draft is kept after its last save
const draftTerm = 7 * day

// draft is a note's unsaved text, and the saves that made it, which the harness
// counts to find a save that went over another
type draft struct {
	Body  string `json:"body"`
	Saves int64  `json:"saves"`
}

// storedDraft is a draft saved before the phase began
type storedDraft struct {
	user, note int64
	draft      draft
	expires    time.Time
}

// draftSave is one save of a tab: the draft it writes and the version it read
type draftSave struct {
	user, note int64
	draft      draft
	read       string
}

// drafts is the draft autosave case on one implementation. A version is text,
// as a page keeps it, and "" is a note without a draft; a save at any version
// but the draft's is a conflict, so that a tab cannot save over a newer draft
type drafts interface {
	preload(ctx context.Context, batch []storedDraft) error
	open(ctx context.Context, user, note int64) (draft, string, error)
	// save returns the version it wrote
	save(ctx context.Context, s draftSave) (string, error)
}

func openDrafts(ctx context.Context, b *backend) (drafts, error) {
	if b.impl == implKV {
		bucket, err := kv.OpenBucket[draft](ctx, b.state, "drafts")
		if err != nil {
			return nil, err
		}
		return &kvDrafts{state: b.state, bucket: bucket}, nil
	}
	b.sweep("drafts", sweepDrafts)
	return &tableDrafts{app: b.app, now: b.now}, nil
}

// tableDrafts is the drafts table an application keeps without kv
type tableDrafts struct {
	app *sqldb.DB
	now func() time.Time
}

const (
	insertDraft = `insert into drafts (note_id, user_id, body, saves, etag, expires_at)
		values (?1, ?2, ?3, ?4, ?5, ?6)`
	selectDraft = `select body, saves, etag from drafts where note_id = ?1 and user_id = ?2 and expires_at > ?3`
	createDraft = `insert into drafts (note_id, user_id, body, saves, etag, expires_at)
		values (?1, ?2, ?3, ?4, ?5, ?6)
		on conflict (note_id) do update set user_id = excluded.user_id, body = excluded.body,
			saves = excluded.saves, etag = excluded.etag, expires_at = excluded.expires_at
			where drafts.expires_at <= ?7`
	saveDraft = `update drafts set body = ?3, saves = ?4, etag = ?5, expires_at = ?6
		where note_id = ?1 and user_id = ?2 and etag = ?7 and expires_at > ?8`
	sweepDrafts = `delete from drafts where expires_at <= ?1`
	countDrafts = `select count(*) from drafts`
)

type draftRow struct {
	Body  string
	Saves int64
	Etag  int64
}

func (t *tableDrafts) preload(ctx context.Context, batch []storedDraft) error {
	return t.app.Tx(ctx, func(tx *sqldb.Tx) error {
		for _, d := range batch {
			_, err := tx.Exec(ctx, insertDraft, d.note, d.user, d.draft.Body, d.draft.Saves, newEtag(),
				d.expires.UnixMilli())
			if err != nil {
				return err
			}
		}
		return nil
	})
}

func (t *tableDrafts) open(ctx context.Context, user, note int64) (draft, string, error) {
	row, err := sqldb.One[draftRow](ctx, t.app, selectDraft, note, user, t.now().UnixMilli())
	switch {
	case errors.Is(err, sql.ErrNoRows):
		return draft{}, "", nil
	case err != nil:
		return draft{}, "", err
	}
	return draft{Body: row.Body, Saves: row.Saves}, etagText(row.Etag), nil
}

func (t *tableDrafts) save(ctx context.Context, s draftSave) (string, error) {
	etag := newEtag()
	result, err := t.write(ctx, s, etag, t.now())
	if err = conflictUnless(result, err); err != nil {
		return "", err
	}
	return etagText(etag), nil
}

// write creates the draft of a tab that read none, taking over one that
// expired, and otherwise writes only over the etag the tab read
func (t *tableDrafts) write(ctx context.Context, s draftSave, etag int64, now time.Time) (sql.Result, error) {
	expires := now.Add(draftTerm).UnixMilli()
	if s.read == "" {
		return t.app.Exec(ctx, createDraft, s.note, s.user, s.draft.Body, s.draft.Saves, etag, expires, now.UnixMilli())
	}
	read, err := parseEtag(s.read)
	if err != nil {
		return nil, err
	}
	return t.app.Exec(ctx, saveDraft, s.note, s.user, s.draft.Body, s.draft.Saves, etag, expires, read,
		now.UnixMilli())
}

// kvDrafts is the draft autosave case through kv
type kvDrafts struct {
	state  *kv.Store
	bucket *kv.Bucket[draft]
}

func (k *kvDrafts) preload(ctx context.Context, batch []storedDraft) error {
	return k.state.Tx(ctx, func(tx *kv.Tx) error {
		inTx := k.bucket.WithTx(tx)
		for _, d := range batch {
			if err := inTx.Of(d.user).Set(ctx, d.note, d.draft, kv.ExpireAt(d.expires)); err != nil {
				return err
			}
		}
		return nil
	})
}

func (k *kvDrafts) open(ctx context.Context, user, note int64) (draft, string, error) {
	entry, found, err := k.bucket.Of(user).GetEntry(ctx, note)
	if err != nil || !found {
		return draft{}, "", err
	}
	return entry.Value, entry.Version.String(), nil
}

// save claims a note without a draft, and writes over a draft only at the
// version the tab read
func (k *kvDrafts) save(ctx context.Context, s draftSave) (string, error) {
	mine := k.bucket.Of(s.user)
	if s.read == "" {
		entry, created, err := mine.SetEntryIfAbsent(ctx, s.note, s.draft, kv.TTL(draftTerm))
		switch {
		case err != nil:
			return "", err
		case !created:
			return "", errMoved
		}
		return entry.Version.String(), nil
	}
	read, err := parseVersion(s.read)
	if err != nil {
		return "", err
	}
	entry, err := mine.SetEntry(ctx, s.note, s.draft, kv.IfVersion(read), kv.TTL(draftTerm))
	if err != nil {
		return "", err
	}
	return entry.Version.String(), nil
}
