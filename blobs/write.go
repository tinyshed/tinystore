package blobs

import (
	"context"
	"fmt"

	"github.com/tinyshed/tinystore"
)

var errNoSource = fmt.Errorf("%w: no live object to copy", tinystore.ErrConflict)

// Delete removes key and its bytes, once no other key names them. An absent
// key is not an error, except to IfMatch, which deletes only the version with
// that ETag.
func (b *Bucket) Delete(ctx context.Context, key string, options ...Option) error {
	c, err := b.begin(key, "Delete", options, deleteTakes)
	if err != nil {
		return b.fail(c, err)
	}
	release, err := b.store.admitWrite(ctx)
	if err != nil {
		return err
	}
	defer release()

	err = b.store.commit(ctx, 0, func(ch *change) error {
		at := address{bucket: b.id, path: c.path}
		existing, readErr := ch.current(at)
		if readErr == nil {
			readErr = c.settings.check(existing, ch.now)
		}
		if readErr != nil || !existing.found {
			return readErr
		}
		return ch.remove(at)
	})
	return b.fail(c, err)
}

// Copy writes the object under from again under to, sharing its bytes: no byte
// is read or written, whatever its size. The copy is a new version with the
// source's type and meta unless the call gives others, and with the expiry a
// Put would give it.
//
// Onto its own key a Copy changes those and keeps the bytes. It takes the
// options Put takes but Size, its conditions for to. An absent source is
// tinystore.ErrConflict.
func (b *Bucket) Copy(ctx context.Context, from, to string, options ...Option) (Object, error) {
	source, target, err := b.beginCopy(from, to, "Copy", options)
	if err != nil {
		return Object{}, err
	}
	return b.copyTo(ctx, source, target, false)
}

// Move is Copy that removes from in the same commit, the bytes untouched.
func (b *Bucket) Move(ctx context.Context, from, to string, options ...Option) (Object, error) {
	source, target, err := b.beginCopy(from, to, "Move", options)
	if err != nil {
		return Object{}, err
	}
	return b.copyTo(ctx, source, target, true)
}

func (b *Bucket) beginCopy(from, to, name string, options []Option) (source, target call, err error) {
	source, err = b.begin(from, name, nil, nil)
	if err != nil {
		return source, target, b.fail(source, err)
	}
	target, err = b.begin(to, name, options, copyTakes)
	return source, target, b.fail(target, err)
}

// copyTo gives target the content source names in one transaction, and
// removes source there too when it moves
func (b *Bucket) copyTo(ctx context.Context, source, target call, move bool) (Object, error) {
	release, err := b.store.admitWrite(ctx)
	if err != nil {
		return Object{}, err
	}
	defer release()

	var copied *version
	err = b.store.commit(ctx, 0, func(ch *change) (err error) {
		copied, err = b.copyIn(ch, source, target, move)
		return err
	})
	if err != nil {
		return Object{}, b.fail(target, err)
	}
	return copied.object(target.key), nil
}

func (b *Bucket) copyIn(ch *change, source, target call, move bool) (*version, error) {
	from := address{bucket: b.id, path: source.path}
	found, err := ch.current(from)
	switch {
	case err != nil:
		return nil, err
	case !found.live(ch.now):
		return nil, b.fail(source, errNoSource)
	}
	copied, err := b.copyOf(found, target.settings, ch.now)
	if err != nil {
		return nil, b.fail(source, err)
	}
	if err = ch.replace(address{bucket: b.id, path: target.path}, copied, target.settings); err != nil {
		return nil, err
	}
	if move && source.path != target.path {
		return copied, ch.remove(from)
	}
	return copied, nil
}

// copyOf is the version a Copy writes: the source's content, and its type and
// meta unless the call names its own
func (b *Bucket) copyOf(found current, settings callSettings, now int64) (*version, error) {
	copied := &version{
		content:     content{id: found.content, size: found.size, etag: found.etag, shared: true},
		contentType: found.contentType, meta: settings.meta, modified: now, expires: b.expiry(settings, now),
	}
	if settings.typed {
		copied.contentType = settings.contentType
	}
	if settings.meta != nil {
		return copied, nil
	}
	var err error
	copied.meta, err = metaOf(found.meta)
	return copied, err
}
