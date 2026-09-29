package blobs

import (
	"bytes"
	"context"
	"crypto/sha256"
	"database/sql"
	"errors"
	"fmt"
	"io/fs"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

var (
	selectOpen = `select o.path, o.size, o.etag, o.type, o.modified, o.expires, o.meta, o.content,
			c.id, c.inline, c.damaged, c.sha256, b.bytes
		from objects as o left join contents as c on c.id = o.content
			left join bodies as b on c.inline and b.id = o.content
		where o.bucket = ?1 and o.path = ?2 and (o.expires is null or o.expires > ?3) and not ` + hidden
	selectStat = `select o.path, o.size, o.etag, o.type, o.modified, o.expires, o.meta from objects as o
		where o.bucket = ?1 and o.path = ?2 and (o.expires is null or o.expires > ?3) and not ` + hidden
)

// Open returns a reader of the object under key as it is at this moment, and
// whether a live object is there. A reader of a file holds the file open and
// nothing of the engine's, so it reads on after its key is deleted, replaced
// or expired, and after the store closes, until its own Close. An inline
// object is checked before its first byte, and a file's read from its first
// byte to its last in order before its last bytes are handed over; a range is
// not checked.
func (b *Bucket) Open(ctx context.Context, key string) (*Reader, bool, error) {
	c, err := b.begin(key, "Open", nil, nil)
	if err != nil {
		return nil, false, b.fail(c, err)
	}
	leave, err := b.store.admit(ctx)
	if err != nil {
		return nil, false, err
	}
	defer leave()
	reserved, err := b.store.reserve(ctx, inlineSize)
	if err != nil {
		return nil, false, b.fail(c, err)
	}

	reader, err := b.open(ctx, c, reserved)
	if err != nil || reader == nil {
		reserved.Release()
		return nil, false, b.fail(c, err)
	}
	return reader, true, nil
}

// open looks the key up and opens what it names. A file gone between the
// lookup and the open went because its key changed, so the key is looked up
// again, three lookups in all; a file that will not open while its key still
// names it is corrupt when it is missing, and the open's error otherwise.
func (b *Bucket) open(ctx context.Context, c call, reserved *tinystore.Reservation) (*Reader, error) {
	var failed int64
	var failure error
	for range openLookups {
		found, err := b.lookup(ctx, c)
		switch {
		case err != nil || found == nil:
			return nil, err
		case found.content == failed && errors.Is(failure, fs.ErrNotExist):
			return nil, fmt.Errorf("%w: the file of content %d is missing", tinystore.ErrCorrupt, failed)
		case found.content == failed:
			return nil, failure
		}
		reader, err := b.readerOf(c, found, reserved)
		if !errors.Is(err, fs.ErrNotExist) && !beingRemoved(err) {
			return reader, err
		}
		failed, failure = found.content, err
		b.store.lookedAgain.Add(1)
	}
	return nil, fmt.Errorf("%w: the key changed %d times while it was opened", tinystore.ErrConflict, openLookups)
}

// opened is what Open reads of an object: its row, and its content's, which
// a missing content row leaves null
type opened struct {
	objectRow
	content int64
	id      sql.NullInt64
	inline  sql.NullBool
	damaged sql.NullBool
	sha256  []byte
	body    []byte
}

func (b *Bucket) lookup(ctx context.Context, c call) (*opened, error) {
	found := &opened{}
	now := b.store.clock()
	err := b.store.file.Lookup(ctx, func(r sqlite.Reader) error {
		return sqlite.QueryRowByKey(ctx, r, selectOpen, b.id, c.path, now).Scan(append(found.fields(), &found.content,
			&found.id, &found.inline, &found.damaged, &found.sha256, &found.body)...)
	})
	if errors.Is(err, sql.ErrNoRows) {
		return nil, nil
	}
	return found, err
}

// readerOf is a reader of what a lookup found: an inline object checked
// against its SHA-256 before its first byte, or its file opened through the
// store's root, which lets the engine remove it under the reader
func (b *Bucket) readerOf(c call, found *opened, reserved *tinystore.Reservation) (*Reader, error) {
	object, err := found.object(b.folder)
	switch {
	case err != nil:
		return nil, err
	case !found.id.Valid:
		return nil, fmt.Errorf("%w: content %d, which the object names, is missing", tinystore.ErrCorrupt,
			found.content)
	case found.damaged.Bool:
		return nil, fmt.Errorf("%w: the scrub found content %d changed or missing", tinystore.ErrCorrupt, found.content)
	}
	reader := &Reader{Object: object, bucket: b.name, path: c.path, want: found.sha256}
	if found.inline.Bool {
		sum := sha256.Sum256(found.body)
		if int64(len(found.body)) != object.Size || !bytes.Equal(sum[:], found.sha256) {
			return nil, fmt.Errorf("%w: the bytes of content %d do not hash to its SHA-256", tinystore.ErrCorrupt,
				found.content)
		}
		reserved.Shrink(int64(len(found.body)))
		reader.body, reader.reserved = found.body, reserved
		return reader, nil
	}
	_, name := objectName(found.content)
	if reader.file, err = b.store.root.Open(name); err != nil {
		return nil, err
	}
	reserved.Release()
	reader.hash = sha256.New()
	return reader, nil
}

// Stat is the object under key without its bytes, and whether a live object
// is there.
func (b *Bucket) Stat(ctx context.Context, key string) (Object, bool, error) {
	c, err := b.begin(key, "Stat", nil, nil)
	if err != nil {
		return Object{}, false, b.fail(c, err)
	}
	leave, err := b.store.admit(ctx)
	if err != nil {
		return Object{}, false, err
	}
	defer leave()

	var found objectRow
	now := b.store.clock()
	err = b.store.file.Lookup(ctx, func(r sqlite.Reader) error {
		return sqlite.QueryRowByKey(ctx, r, selectStat, b.id, c.path, now).Scan(found.fields()...)
	})
	if errors.Is(err, sql.ErrNoRows) {
		return Object{}, false, nil
	}
	if err != nil {
		return Object{}, false, b.fail(c, err)
	}
	object, err := found.object(b.folder)
	return object, err == nil, b.fail(c, err)
}

// current is the key's row as a lookup finds it, for a condition an upload
// checks before its first byte
func (b *Bucket) current(ctx context.Context, c call) (current, error) {
	found := current{found: true}
	err := b.store.file.Lookup(ctx, func(r sqlite.Reader) error {
		return sqlite.QueryRowByKey(ctx, r, selectCurrent, b.id, c.path).Scan(found.fields()...)
	})
	if errors.Is(err, sql.ErrNoRows) {
		return current{}, nil
	}
	return found, err
}
