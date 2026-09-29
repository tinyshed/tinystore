package blobs

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"sync"

	"github.com/tinyshed/tinystore"
)

// errEnded is an upload used after its Commit or Abort
var errEnded = fmt.Errorf("blobs: the upload has ended: %w", tinystore.ErrClosed)

// Commit makes the object appear under its key, whole, and returns it once it
// is durable: its file synced and renamed into objects/ with its directory
// synced, then its row committed beside the writes of other goroutines. A
// stream shorter than its Size is tinystore.ErrInvalid, and a condition that
// no longer holds tinystore.ErrConflict; either leaves nothing.
func (u *Upload) Commit(ctx context.Context) (Object, error) {
	u.mu.Lock()
	defer u.mu.Unlock()
	if err := u.alive(); err != nil {
		return Object{}, err
	}

	object, err := u.commit(ctx)
	u.finish(ctx, err)
	return object, u.bucket.fail(u.call, err)
}

func (u *Upload) commit(ctx context.Context) (Object, error) {
	if declared := u.call.settings.size; declared >= 0 && u.written != declared {
		return Object{}, fmt.Errorf("%w: a stream of %d bytes, shorter than its Size of %d", tinystore.ErrInvalid,
			u.written, declared)
	}
	sum := u.hash.Sum(nil)
	placed := content{size: u.written, etag: sum[:etagBytes], sha256: sum}
	var err error
	if u.at == inMemory {
		err = u.placeInline(&placed)
	} else {
		err = u.placeFile(&placed)
	}
	if err != nil {
		return Object{}, err
	}
	return u.bucket.commitUpload(ctx, u.call, placed)
}

// placeInline gives an inline object its id; its bytes go in its row
func (u *Upload) placeInline(placed *content) error {
	id, err := u.bucket.store.takeID(u.ctx)
	u.id, placed.id, placed.body, placed.inline = id, id, u.buffer, true
	return err
}

// placeFile syncs and closes the upload's file and renames it into objects/,
// then syncs that directory, so that the commit names bytes whose name is
// durable. The file is never written again, nor given another name.
func (u *Upload) placeFile(placed *content) error {
	s := u.bucket.store
	s.reach(stepWritten)
	err := u.file.Sync()
	err = errors.Join(err, u.file.Close())
	u.file = nil
	if err != nil {
		return fmt.Errorf("blobs: sync %s: %w", uploadName(u.id), err)
	}
	s.reach(stepSynced)

	dir, name := objectName(u.id)
	if err = s.makeDir(dir); err != nil {
		return err
	}
	if err = s.rename(uploadName(u.id), name); err != nil {
		return err
	}
	u.at = inObjects
	if err = s.syncShared(dir); err != nil {
		return err
	}
	s.reach(stepPublished)
	placed.id = u.id
	return nil
}

// commitUpload commits an upload's object under its key, beside the writes of
// other goroutines. It checks the condition again, writes the content and the
// row, and lets go of the content the key named before.
func (b *Bucket) commitUpload(ctx context.Context, c call, placed content) (Object, error) {
	free, err := b.store.writes.Take(ctx)
	if err != nil {
		return Object{}, err
	}
	defer free()

	v := &version{content: placed, contentType: c.settings.contentType, meta: c.settings.meta}
	err = b.store.commit(ctx, len(placed.body), func(ch *change) error {
		v.modified, v.expires = ch.now, b.expiry(c.settings, ch.now)
		return ch.replace(address{bucket: b.id, path: c.path}, v, c.settings)
	})
	return v.object(c.key), err
}

// finish ends the upload after its commit. A committed one lets its id go, a
// failed one removes its bytes, and one whose outcome is unknown asks the file
// whether its content is there before it removes them.
func (u *Upload) finish(ctx context.Context, err error) {
	s := u.bucket.store
	switch {
	case err == nil:
		s.letGo(u.id)
	case errors.Is(err, ErrOutcomeUnknown) && u.at == inObjects:
		s.resolve(context.WithoutCancel(ctx), u.id)
	default:
		u.removeBytes()
	}
	u.release(errEnded)
}

// Abort ends the upload and leaves nothing; after Commit it does nothing.
func (u *Upload) Abort() {
	u.abort(errEnded)
}

// abort ends the upload from any goroutine: its own, maintenance's once its
// context has ended, or the store's Close. It says whether it was the one that
// ended it.
func (u *Upload) abort(reason error) bool {
	u.mu.Lock()
	defer u.mu.Unlock()
	if u.ended != nil {
		return false
	}
	u.removeBytes()
	u.release(reason)
	return true
}

// end finishes an upload that failed while its caller held it
func (u *Upload) end(reason error) {
	u.removeBytes()
	u.release(reason)
}

// removeBytes removes the upload's file wherever it lies and lets its id go. A
// file of objects/ that will not go keeps its id held, so that the settled mark
// stays below it until maintenance or the next open removes it.
func (u *Upload) removeBytes() {
	s := u.bucket.store
	var err error
	switch u.at {
	case inUploads:
		if u.file != nil {
			err = u.file.Close()
			u.file = nil
		}
		if removeErr := s.root.Remove(uploadName(u.id)); !errors.Is(removeErr, fs.ErrNotExist) {
			err = errors.Join(err, removeErr)
		}
	case inObjects:
		if err = s.removeFile(u.id); err != nil {
			s.doubt(u.id)
		}
	}
	if err != nil {
		s.removals.observe(s.now(), err)
	}
	if u.id != 0 && (err == nil || u.at != inObjects) {
		s.letGo(u.id)
	}
}

// release gives back what the upload holds and says why it ended
func (u *Upload) release(reason error) {
	u.ended = reason
	u.bucket.store.live.remove(u)
	u.reserved.Release()
	u.leave()
}

// liveUploads is every upload that has not ended, which Close aborts; once
// closed it takes no more
type liveUploads struct {
	mu     sync.Mutex
	set    map[*Upload]bool
	closed bool
}

func (l *liveUploads) add(u *Upload) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.closed {
		return false
	}
	l.set[u] = true
	return true
}

func (l *liveUploads) remove(u *Upload) {
	l.mu.Lock()
	defer l.mu.Unlock()
	delete(l.set, u)
}

// abortAll aborts every upload that has not ended, outside the lock, since
// each abort waits for its upload's own call to return
func (l *liveUploads) abortAll(reason error) {
	l.mu.Lock()
	l.closed = true
	l.mu.Unlock()
	for _, u := range l.uploads() {
		u.abort(reason)
	}
}

// abortEnded aborts the uploads whose context has ended while no call held
// them, and returns how many
func (l *liveUploads) abortEnded() int {
	aborted := 0
	for _, u := range l.uploads() {
		if u.ctx.Err() != nil && u.abort(context.Cause(u.ctx)) {
			aborted++
		}
	}
	return aborted
}

func (l *liveUploads) uploads() []*Upload {
	l.mu.Lock()
	defer l.mu.Unlock()
	uploads := make([]*Upload, 0, len(l.set))
	for u := range l.set {
		uploads = append(uploads, u)
	}
	return uploads
}
