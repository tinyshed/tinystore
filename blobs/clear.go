package blobs

import (
	"context"
	"database/sql"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

const (
	countUnder = `select count(*) from (
		select 1 from objects where bucket = ?1 and path >= ?2 and path < ?3 limit cast(?4 as integer)
	)`
	deleteUnder = `delete from objects where bucket = ?1 and path >= ?2 and path < ?3 returning content`
	markCleared = `insert into cleared (bucket, prefix, revision) values (?1, ?2, ?3)
		on conflict (bucket, prefix) do update set revision = excluded.revision`
)

// Clear removes every object of this handle's folder and of the folders under
// it, at once. Up to 10,000 objects go in one transaction; a larger folder is
// marked cleared at the file's revision, which hides its objects from every
// call the moment it commits, and maintenance deletes them 10,000 a
// transaction. An object written after the Clear is a new object and stays.
func (b *Bucket) Clear(ctx context.Context) error {
	if b.err != nil {
		return b.err
	}
	release, err := b.store.admitWrite(ctx)
	if err != nil {
		return err
	}
	defer release()

	var freed []int64
	err = b.store.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		ch := &change{ctx: ctx, w: w, store: b.store, now: b.store.clock()}
		clearErr := b.clearIn(ch)
		freed = ch.freed
		return clearErr
	})
	if err == nil {
		b.store.letFilesGo(freed)
	}
	return b.fail(call{path: b.folder}, err)
}

// clearIn deletes the folder's objects in the transaction, or marks the
// folder when they are more than a transaction should hold the writer for
func (b *Bucket) clearIn(ch *change) error {
	from, to := b.folder, prefixEnd(b.folder)
	var objects int
	err := sqlite.QueryRow(ch.ctx, ch.w, countUnder, b.id, from, to, b.store.clearBound+1).Scan(&objects)
	if err != nil {
		return err
	}
	if objects <= b.store.clearBound {
		contents, deleteErr := ch.deleted(deleteUnder, b.id, from, to)
		if deleteErr != nil {
			return deleteErr
		}
		return ch.releaseAll(contents)
	}
	revision, err := b.store.nextRevision(ch.ctx, ch.w)
	if err == nil {
		_, err = ch.w.ExecContext(ch.ctx, markCleared, b.id, b.folder, revision)
	}
	return err
}

// deleted runs a delete that returns the content of each object it deleted
func (ch *change) deleted(query string, args ...any) ([]int64, error) {
	rows, err := ch.w.QueryContext(ch.ctx, query, args...) //nolint:rowserrcheck // EachRow checks Err
	if err != nil {
		return nil, err
	}
	var contents []int64
	err = sqlite.EachRow(rows, "deleted objects", func(rows *sql.Rows) error {
		var id int64
		scanErr := rows.Scan(&id)
		contents = append(contents, id)
		return scanErr
	})
	return contents, err
}
