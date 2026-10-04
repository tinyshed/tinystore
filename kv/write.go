package kv

import (
	"context"
	"database/sql"
	"errors"
	"fmt"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

var (
	selectCell = `select version, expires, spill, ` + hidden("cells", 3) + `
		from _tinystore_kv_cells as cells where bucket = ?1 and path = ?2`
	takeCell = `delete from _tinystore_kv_cells as cells
		where bucket = ?1 and path = ?2 and (expires is null or expires > ?3) and (?4 = 0 or version = ?4)
			and not ` + hidden("cells", 5) + `
		returning version, expires, value, spill`
	deleteCell = `delete from _tinystore_kv_cells as cells
		where bucket = ?1 and path = ?2
			and (?4 = 0 or (version = ?4 and (expires is null or expires > ?3) and not ` + hidden("cells", 5) + `))
		returning spill`
	touchCell = `update _tinystore_kv_cells as cells set expires = ?4
		where bucket = ?1 and path = ?2 and (expires is null or expires > ?3) and (?5 = 0 or version = ?5)
			and not ` + hidden("cells", 6) + `
		returning version`
)

const (
	upsertCell = `insert into _tinystore_kv_cells as cells (bucket, path, version, expires, value, spill)
		values (?1, ?2, ?3, ?4, ?5, ?6)
		on conflict (bucket, path) do update set
			version = excluded.version, expires = excluded.expires, value = excluded.value, spill = excluded.spill`
	insertSpilled = `insert into _tinystore_kv_spilled as spilled (value) values (?1)`
	deleteSpilled = `delete from _tinystore_kv_spilled as spilled where id = ?1`
	takeSpilled   = `delete from _tinystore_kv_spilled as spilled where id = ?1 returning value`
)

var errVersionMoved = fmt.Errorf("%w: the key is not live at the version given", tinystore.ErrConflict)

// Set writes value under key. A key it creates, or one that had expired, takes
// the bucket's DefaultTTL unless kv.TTL or kv.ExpireAt says otherwise; a live
// key keeps the expiry it has unless one of them does.
func (b *Bucket[V]) Set(ctx context.Context, key any, value V, options ...Option) error {
	_, err := b.SetEntry(ctx, key, value, options...)
	return err
}

// SetEntry is Set, returning what it wrote with its new version.
func (b *Bucket[V]) SetEntry(ctx context.Context, key any, value V, options ...Option) (Entry[V], error) {
	c, err := b.begin(key, options)
	if err != nil {
		return Entry[V]{}, b.fail(c, err)
	}

	entered, kept, err := b.enterWith(ctx, value, 0)
	if err != nil {
		return Entry[V]{}, b.fail(c, err)
	}
	defer entered.leave()

	var written cellWrite
	err = b.commit(ctx, kept.size(), func(w sqlite.Writer) (writeErr error) {
		written, writeErr = b.setCell(ctx, w, c, kept)
		return writeErr
	})
	if err != nil {
		return Entry[V]{}, b.fail(c, err)
	}

	return entryWritten(c.key, value, written), nil
}

// SetIfAbsent writes value under key only if no live key is there, and says
// whether it did; an expired key is absent.
func (b *Bucket[V]) SetIfAbsent(ctx context.Context, key any, value V, options ...Option) (bool, error) {
	_, created, err := b.SetEntryIfAbsent(ctx, key, value, options...)
	return created, err
}

// SetEntryIfAbsent is SetIfAbsent, returning the entry it wrote or the one
// that was there, each with its version.
func (b *Bucket[V]) SetEntryIfAbsent(ctx context.Context, key any, value V, options ...Option) (
	Entry[V], bool, error,
) {
	c, err := b.begin(key, options)
	if err == nil && c.options.version.revision != 0 {
		err = fmt.Errorf("%w: IfVersion with SetIfAbsent", tinystore.ErrInvalid)
	}
	if err != nil {
		return Entry[V]{}, false, b.fail(c, err)
	}

	entered, kept, err := b.enterWith(ctx, value, maxValue)
	if err != nil {
		return Entry[V]{}, false, b.fail(c, err)
	}
	defer entered.leave()

	var there row
	var written cellWrite
	created := false
	err = b.commit(ctx, kept.size(), func(w sqlite.Writer) error {
		var found bool
		var writeErr error
		there, found, writeErr = readLive(ctx, w, b.id, c)
		if writeErr != nil || found {
			return writeErr
		}
		created = true
		written, writeErr = b.setCell(ctx, w, c, kept)
		return writeErr
	})
	switch {
	case err != nil:
		return Entry[V]{}, false, b.fail(c, err)
	case created:
		return entryWritten(c.key, value, written), true, nil
	}
	entry, err := b.entryOf(c.key, there)
	return entry, false, b.fail(c, err)
}

// Take reads the value under key and deletes it in one step, so that of the
// callers taking one key only one gets it. A value that no longer decodes is
// ErrCorrupt and stays where it was, inside Tx as well.
func (b *Bucket[V]) Take(ctx context.Context, key any, options ...Option) (V, bool, error) {
	c, err := b.begin(key, options)
	if err != nil {
		return zeroFound[V](b.fail(c, err))
	}

	entered, err := b.enter(ctx, maxValue)
	if err != nil {
		return zeroFound[V](b.fail(c, err))
	}
	defer entered.leave()

	var entry Entry[V]
	found := false
	err = b.commitAlone(ctx, func(w sqlite.Writer) error {
		taken, there, takeErr := takeRow(ctx, w, b.id, c)
		if takeErr != nil || !there {
			return takeErr
		}
		entry, takeErr = b.entryInWriter(c.key, taken)
		found = takeErr == nil
		return takeErr
	})
	if err != nil || !found {
		return zeroFound[V](b.fail(c, err))
	}

	return entry.Value, true, nil
}

// Delete removes key; an absent key is not an error, except to IfVersion.
func (b *Bucket[V]) Delete(ctx context.Context, key any, options ...Option) error {
	c, err := b.begin(key, options)
	if err != nil {
		return b.fail(c, err)
	}

	err = b.write(ctx, 0, func(w sqlite.Writer) error { return b.deleteIn(ctx, w, c) })
	return b.fail(c, err)
}

// deleteIn deletes the key c names with w, and the row its value spilled to.
func (b *branch) deleteIn(ctx context.Context, w sqlite.Writer, c call) error {
	var spill sql.NullInt64
	err := sqlite.QueryRowByKey(ctx, w, deleteCell, c.args(b.id, c.path, c.now,
		c.options.version.revision)...).Scan(&spill)
	if errors.Is(err, sql.ErrNoRows) {
		return c.absent()
	}
	if err != nil {
		return err
	}
	return dropSpilled(ctx, w, spill)
}

// Touch gives a live key a new expiry, kv.TTL's, kv.ExpireAt's or else the
// bucket's DefaultTTL, and keeps its value and version, so that a renewal
// fails no IfVersion. It says whether the key was there.
func (b *Bucket[V]) Touch(ctx context.Context, key any, options ...Option) (bool, error) {
	c, err := b.begin(key, options)
	if err == nil && !c.options.hasExpiry() && b.ttl <= 0 {
		err = fmt.Errorf("%w: Touch needs kv.TTL or kv.ExpireAt, or the bucket's DefaultTTL", tinystore.ErrInvalid)
	}
	if err != nil {
		return false, b.fail(c, err)
	}

	expires := b.expiresFor(c, cell{})
	found := false
	err = b.write(ctx, 0, func(w sqlite.Writer) error {
		var version int64
		touchErr := sqlite.QueryRowByKey(ctx, w, touchCell, c.args(b.id, c.path, c.now, expires,
			c.options.version.revision)...).Scan(&version)
		if errors.Is(touchErr, sql.ErrNoRows) {
			return c.absent()
		}
		found = touchErr == nil
		return touchErr
	})
	return found, b.fail(c, err)
}

// enterWith takes a write's place and the memory its value's row may hold,
// beside extra for a value it may read, then encodes the value and keeps of
// that memory what the row holds. A value its type measures over 1 MiB is
// refused before anything is made.
func (b *Bucket[V]) enterWith(ctx context.Context, value V, extra int) (place, stored, error) {
	return b.keepValue(ctx, value, extra, b.enter)
}

func (b *Bucket[V]) keepValue(
	ctx context.Context, value V, extra int, enter func(context.Context, int) (place, error),
) (place, stored, error) {
	weight := b.codec.weigh(value)
	if weight > maxValue {
		return place{}, stored{}, tooLarge(weight)
	}
	entered, err := enter(ctx, weight+extra)
	if err != nil {
		return place{}, stored{}, err
	}

	encoded, err := b.codec.encode(value)
	var kept stored
	if err == nil {
		kept, err = keep(encoded)
	}
	if err != nil {
		entered.leave()
		return place{}, stored{}, err
	}
	entered.keep(kept.size() + extra)
	return entered, kept, nil
}

// cell is a key's row as a write finds it, hidden when a marked Clear hid it
type cell struct {
	found   bool
	hidden  bool
	version int64
	expires sql.NullInt64
	spill   sql.NullInt64
}

func (c cell) live(now int64) bool {
	return c.found && !c.hidden && (!c.expires.Valid || c.expires.Int64 > now)
}

type cellWrite struct {
	version int64
	expires sql.NullInt64
}

func entryWritten[V any](key string, value V, written cellWrite) Entry[V] {
	return Entry[V]{
		Key: key, Value: value, Version: Version{revision: written.version},
		ExpiresAt: expiryTime(written.expires.Int64, written.expires.Valid),
	}
}

// setCell writes a value in the writer's transaction: the key's row as it is
// decides the expiry and, with IfVersion, whether the write happens at all
func (b *Bucket[V]) setCell(ctx context.Context, w sqlite.Writer, c call, value stored) (cellWrite, error) {
	existing, err := readCell(ctx, w, b.id, c)
	if err == nil {
		err = c.checkVersion(existing)
	}
	if err != nil {
		return cellWrite{}, err
	}

	version, err := b.state.nextRevision(ctx, w)
	if err != nil {
		return cellWrite{}, err
	}
	spill, err := writeSpilled(ctx, w, value.spill)
	if err != nil {
		return cellWrite{}, err
	}

	expires := b.expiresFor(c, existing)
	if _, err = w.ExecContext(ctx, upsertCell, b.id, c.path, version, expires, value.inline, spill); err != nil {
		return cellWrite{}, err
	}
	return cellWrite{version: version, expires: expires}, dropSpilled(ctx, w, existing.spill)
}

func readCell(ctx context.Context, w sqlite.Writer, bucket int64, c call) (cell, error) {
	found := cell{found: true}
	err := sqlite.QueryRowByKey(ctx, w, selectCell, c.args(bucket, c.path)...).
		Scan(&found.version, &found.expires, &found.spill, &found.hidden)
	if errors.Is(err, sql.ErrNoRows) {
		return cell{}, nil
	}
	return found, err
}

// checkVersion refuses a write whose IfVersion names a version the key does
// not have, or a key that is not live
func (c call) checkVersion(existing cell) error {
	if c.options.version.revision == 0 {
		return nil
	}
	if !existing.live(c.now) || existing.version != c.options.version.revision {
		return errVersionMoved
	}
	return nil
}

// absent is a key a write found missing: nothing to do, or, with IfVersion, a
// conflict
func (c call) absent() error {
	if c.options.version.revision != 0 {
		return errVersionMoved
	}
	return nil
}

// writeSpilled keeps a value over inlineLimit in a row of its own
func writeSpilled(ctx context.Context, w sqlite.Writer, value []byte) (sql.NullInt64, error) {
	if value == nil {
		return sql.NullInt64{}, nil
	}
	result, err := w.ExecContext(ctx, insertSpilled, value)
	if err != nil {
		return sql.NullInt64{}, err
	}
	id, err := result.LastInsertId()
	return sql.NullInt64{Int64: id, Valid: err == nil}, err
}

func dropSpilled(ctx context.Context, w sqlite.Writer, spill sql.NullInt64) error {
	if !spill.Valid {
		return nil
	}
	_, err := w.ExecContext(ctx, deleteSpilled, spill.Int64)
	return err
}

// takeRow deletes a live key's row and its spilled value, and returns them
func takeRow(ctx context.Context, w sqlite.Writer, bucket int64, c call) (row, bool, error) {
	taken := row{path: c.path}
	err := sqlite.QueryRowByKey(ctx, w, takeCell, c.args(bucket, c.path, c.now, c.options.version.revision)...).
		Scan(&taken.version, &taken.expires, &taken.value, &taken.spill)
	switch {
	case errors.Is(err, sql.ErrNoRows):
		return row{}, false, c.absent()
	case err != nil || !taken.spill.Valid:
		return taken, err == nil, err
	}
	err = sqlite.QueryRowByKey(ctx, w, takeSpilled, taken.spill.Int64).Scan(&taken.spilled)
	if errors.Is(err, sql.ErrNoRows) {
		err = fmt.Errorf("%w: the spilled value %d is missing", tinystore.ErrCorrupt, taken.spill.Int64)
	}
	return taken, err == nil, err
}

func zeroFound[V any](err error) (V, bool, error) {
	var zero V
	return zero, false, err
}
