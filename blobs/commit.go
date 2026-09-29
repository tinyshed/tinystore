package blobs

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"maps"
	"slices"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// address is where an object's row is: its bucket and its whole path
type address struct {
	bucket int64
	path   string
}

// content is the bytes a write names: new ones with their hash, and the bytes
// themselves when inline, or ones another object names, which a Copy shares
type content struct {
	id     int64
	size   int64
	etag   []byte // the first bytes of sha256, which objects keep
	sha256 []byte // a new content's; a shared one keeps its own
	body   []byte // an inline content's bytes
	inline bool
	shared bool
}

// version is an object's row as a write gives it
type version struct {
	content     content
	revision    int64
	contentType string
	modified    int64
	expires     sql.NullInt64
	meta        map[string]string
}

// object is the version as a caller sees it, under key
func (v *version) object(key string) Object {
	object := Object{
		Key: key, Size: v.content.size, ETag: etagOf(v.content.etag), ContentType: v.contentType,
		Modified: time.UnixMilli(v.modified), Meta: v.meta,
	}
	if v.expires.Valid {
		object.Expires = time.UnixMilli(v.expires.Int64)
	}
	return object
}

// current is a key's row as a write finds it, hidden when a marked Clear hid it
type current struct {
	found       bool
	hidden      bool
	content     int64
	size        int64
	etag        []byte
	contentType string
	expires     sql.NullInt64
	meta        sql.NullString
}

func (c current) live(now int64) bool {
	return c.found && !c.hidden && (!c.expires.Valid || c.expires.Int64 > now)
}

var (
	errExists = fmt.Errorf("%w: the key holds a live object", tinystore.ErrConflict)
	errMoved  = fmt.Errorf("%w: the key holds no live object with the ETag given", tinystore.ErrConflict)
)

// check refuses a write whose condition the key's row does not meet
func (s callSettings) check(existing current, now int64) error {
	live := existing.live(now)
	switch {
	case s.ifNoneMatch && live:
		return errExists
	case s.ifMatch != "" && (!live || !matches(s.ifMatch, etagOf(existing.etag))):
		return errMoved
	}
	return nil
}

// change is one write's statements in its transaction, and the files of the
// contents it leaves without names, which go once it commits
type change struct {
	ctx   context.Context
	w     sqlite.Writer
	store *Store
	now   int64 // the store's clock in the transaction, unix milliseconds
	freed []int64
}

// commit runs a write in a group's transaction beside the writes of other
// goroutines, bytes weighing it against the group's bound, and removes the
// files it left without names once it has committed. A write that has begun
// finishes with its group, whatever its caller's context does.
func (s *Store) commit(ctx context.Context, bytes int, write func(*change) error) error {
	var freed []int64
	err := s.file.UpdateGrouped(ctx, bytes, func(w sqlite.Writer) error {
		ch := &change{ctx: context.WithoutCancel(ctx), w: w, store: s, now: s.clock()}
		if err := write(ch); err != nil {
			return err
		}
		freed = ch.freed
		return nil
	})
	if err != nil {
		return err
	}
	s.reach(stepCommitted)
	s.letFilesGo(freed)
	return nil
}

var selectCurrent = `select o.content, o.size, o.etag, o.type, o.expires, o.meta, ` + hidden + `
	from objects as o where o.bucket = ?1 and o.path = ?2`

func (ch *change) current(of address) (current, error) {
	found := current{found: true}
	err := sqlite.QueryRowByKey(ch.ctx, ch.w, selectCurrent, of.bucket, of.path).Scan(found.fields()...)
	if errors.Is(err, sql.ErrNoRows) {
		return current{}, nil
	}
	return found, err
}

// fields are where a statement of selectCurrent's columns scans them
func (c *current) fields() []any {
	return []any{&c.content, &c.size, &c.etag, &c.contentType, &c.expires, &c.meta, &c.hidden}
}

// replace writes a version over whatever the key holds, if the call's
// condition holds against it, and takes a name from the content it named
func (ch *change) replace(to address, v *version, condition callSettings) error {
	existing, err := ch.current(to)
	if err == nil {
		err = condition.check(existing, ch.now)
	}
	if err != nil {
		return err
	}
	if v.revision, err = ch.store.nextRevision(ch.ctx, ch.w); err != nil {
		return err
	}
	if err = ch.name(v.content); err != nil {
		return err
	}
	if err = ch.write(to, v); err != nil || !existing.found {
		return err
	}
	return ch.release(existing.content, 1)
}

const (
	insertContent = `insert into contents (id, names, size, sha256, inline) values (?1, 1, ?2, ?3, ?4)`
	insertBody    = `insert into bodies (id, bytes) values (?1, ?2)`
	nameContent   = `update contents set names = names + 1 where id = ?1 returning names`
)

// name gives a content the name the write makes: new bytes are written, the
// body with them when inline, and a shared content counts one more
func (ch *change) name(c content) error {
	if c.shared {
		var names int64
		err := sqlite.QueryRowByKey(ch.ctx, ch.w, nameContent, c.id).Scan(&names)
		if errors.Is(err, sql.ErrNoRows) {
			return fmt.Errorf("%w: content %d, which an object names, is missing", tinystore.ErrCorrupt, c.id)
		}
		return err
	}
	if _, err := ch.w.ExecContext(ch.ctx, insertContent, c.id, c.size, c.sha256, c.inline); err != nil || !c.inline {
		return err
	}
	_, err := ch.w.ExecContext(ch.ctx, insertBody, c.id, c.body)
	return err
}

const upsertObject = `insert into objects (bucket, path, revision, content, size, etag, type, modified, expires, meta)
	values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
	on conflict (bucket, path) do update set revision = excluded.revision, content = excluded.content,
		size = excluded.size, etag = excluded.etag, type = excluded.type, modified = excluded.modified,
		expires = excluded.expires, meta = excluded.meta`

func (ch *change) write(to address, v *version) error {
	meta, err := metaText(v.meta)
	if err != nil {
		return fmt.Errorf("%w: meta JSON cannot write: %w", tinystore.ErrInvalid, err)
	}
	_, err = ch.w.ExecContext(ch.ctx, upsertObject, to.bucket, to.path, v.revision, v.content.id, v.content.size,
		v.content.etag, v.contentType, v.modified, v.expires, meta)
	return err
}

const deleteObject = `delete from objects where bucket = ?1 and path = ?2 returning content`

// remove deletes a key's row, whatever it holds, and takes a name from its content
func (ch *change) remove(at address) error {
	var id int64
	err := sqlite.QueryRowByKey(ch.ctx, ch.w, deleteObject, at.bucket, at.path).Scan(&id)
	if errors.Is(err, sql.ErrNoRows) {
		return nil
	}
	if err != nil {
		return err
	}
	return ch.release(id, 1)
}

const (
	unnameContent = `update contents set names = names - ?2 where id = ?1 returning names, inline`
	dropContent   = `delete from contents where id = ?1`
	dropBody      = `delete from bodies where id = ?1`
)

// release takes count names from a content: one left without names goes, an
// inline one here with its bytes, a file's once the write has committed
func (ch *change) release(id int64, count int) error {
	var names int64
	var inline bool
	err := sqlite.QueryRowByKey(ch.ctx, ch.w, unnameContent, id, count).Scan(&names, &inline)
	switch {
	case errors.Is(err, sql.ErrNoRows):
		return fmt.Errorf("%w: content %d, which an object named, is missing", tinystore.ErrCorrupt, id)
	case err != nil || names > 0:
		return err
	case names < 0:
		return fmt.Errorf("%w: content %d lost more names than it had", tinystore.ErrCorrupt, id)
	case !inline:
		ch.freed = append(ch.freed, id)
		return nil
	}
	if _, err = ch.w.ExecContext(ch.ctx, dropBody, id); err != nil {
		return err
	}
	_, err = ch.w.ExecContext(ch.ctx, dropContent, id)
	return err
}

// releaseAll takes a name from the content of each object a batch deleted,
// a statement a content however many of its objects the batch held
func (ch *change) releaseAll(contents []int64) error {
	counts := map[int64]int{}
	for _, id := range contents {
		counts[id]++
	}
	for _, id := range slices.Sorted(maps.Keys(counts)) {
		if err := ch.release(id, counts[id]); err != nil {
			return err
		}
	}
	return nil
}

// expiry is the expiry a write gives its object: the call's, else the
// bucket's default from now, else none
func (b *Bucket) expiry(settings callSettings, now int64) sql.NullInt64 {
	expires, valid := settings.expires(now, b.defaultTTL)
	return sql.NullInt64{Int64: expires, Valid: valid}
}
