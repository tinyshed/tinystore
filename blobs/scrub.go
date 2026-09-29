package blobs

import (
	"bytes"
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding"
	"errors"
	"fmt"
	"hash"
	"io"
	"io/fs"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// scrubPlace is where the scrub is: the content it reads and how far, the
// hash of that content's bytes before there, and the bytes a slice reads in
// this pass; a zero pace begins a pass
type scrubPlace struct {
	content int64
	offset  int64
	state   []byte
	pace    int64
}

// scrubbed is a content as the scrub reads it
type scrubbed struct {
	id     int64
	size   int64
	sha256 []byte
	inline bool
}

// scrubbing is one slice of the scrub: where it is, what it has read, what it found
type scrubbing struct {
	store   *Store
	at      scrubPlace
	buffer  []byte
	read    int64
	damaged int
}

const (
	selectScrub = `select content, offset, state, pace from scrub`
	updateScrub = `update scrub set content = ?1, offset = ?2, state = ?3, pace = ?4`
	namedBytes  = `select coalesce(sum(size), 0) from contents where names > 0`
	nextScrub   = `select id, size, sha256, inline from contents
		where id >= ?1 and names > 0 and damaged = 0 order by id limit 1`
)

// scrub reads a slice of the contents, a 43,200th of their bytes a call and
// at least 1 MiB, so that a pass at a call a minute takes thirty days, and
// keeps its place and its hash's state in blobs.db, so that a restart does
// not begin the pass again. Its buffer waits for no memory: when the store's
// is taken, the slice waits for the next call.
func (s *Store) scrub(ctx context.Context) (read int64, damaged int, err error) {
	hold := s.scrubHold()
	reserved, err := s.runtime.ReserveNow(int64(hold))
	if err != nil {
		return 0, 0, nil // a slice for which the store's memory is taken waits for the next call
	}
	defer reserved.Release()

	pass := &scrubbing{store: s, buffer: make([]byte, hold)}
	err = s.file.Lookup(ctx, func(r sqlite.Reader) error {
		return sqlite.QueryRowByKey(ctx, r, selectScrub).Scan(&pass.at.content, &pass.at.offset, &pass.at.state,
			&pass.at.pace)
	})
	if err == nil {
		err = pass.slice(ctx)
	}
	if err == nil {
		err = s.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
			_, execErr := w.ExecContext(ctx, updateScrub, pass.at.content, pass.at.offset, pass.at.state, pass.at.pace)
			return execErr
		})
	}
	return pass.read, pass.damaged, err
}

// slice reads contents in the order of their ids from the scrub's place
// until it has read its pace, and begins a pass when the last one ended
func (p *scrubbing) slice(ctx context.Context) error {
	if p.at.pace == 0 {
		total, err := p.store.namedBytes(ctx)
		if err != nil {
			return err
		}
		p.at = scrubPlace{pace: max(total/int64(scrubPass/maintainEvery), scrubBuffer)}
	}
	for p.read < p.at.pace {
		next, found, err := p.store.nextToScrub(ctx, p.at.content)
		if err != nil {
			return err
		}
		if !found {
			p.at = scrubPlace{}
			return nil
		}
		if next.id != p.at.content {
			p.at = scrubPlace{content: next.id, pace: p.at.pace}
		}
		if err = p.readContent(ctx, next); err != nil {
			return err
		}
	}
	return nil
}

// scrubHold is what the scrub reads at once: 1 MiB, or a quarter of the
// store's memory when that is less
func (s *Store) scrubHold() int {
	if budget := s.runtime.Memory().Capacity; budget > 0 {
		return int(max(min(scrubBuffer, budget/4), 1))
	}
	return scrubBuffer
}

func (s *Store) namedBytes(ctx context.Context) (int64, error) {
	var total int64
	err := s.file.Lookup(ctx, func(r sqlite.Reader) error {
		return sqlite.QueryRow(ctx, r, namedBytes).Scan(&total)
	})
	return total, err
}

func (s *Store) nextToScrub(ctx context.Context, from int64) (scrubbed, bool, error) {
	var next scrubbed
	err := s.file.Lookup(ctx, func(r sqlite.Reader) error {
		return sqlite.QueryRow(ctx, r, nextScrub, from).Scan(&next.id, &next.size, &next.sha256, &next.inline)
	})
	if errors.Is(err, sql.ErrNoRows) {
		return scrubbed{}, false, nil
	}
	return next, err == nil, err
}

// readContent reads on through one content, as far as the slice's pace
// allows, and judges it once it has read its last byte
func (p *scrubbing) readContent(ctx context.Context, next scrubbed) error {
	sum, err := p.hashFrom(next)
	if err != nil {
		return err
	}
	for p.at.offset < next.size && p.read < p.at.pace {
		n, readErr := p.readAt(ctx, next, min(int64(len(p.buffer)), next.size-p.at.offset))
		sum.Write(p.buffer[:n])
		p.at.offset += int64(n)
		p.read += int64(n)
		if errors.Is(readErr, fs.ErrNotExist) || errors.Is(readErr, io.EOF) {
			return p.judge(ctx, next, "missing or shorter than its size")
		}
		if readErr != nil {
			return readErr
		}
	}
	if p.at.offset < next.size {
		p.at.state, err = stateOf(sum)
		return err
	}
	if !bytes.Equal(sum.Sum(nil), next.sha256) {
		return p.judge(ctx, next, "changed")
	}
	p.at = scrubPlace{content: next.id + 1, pace: p.at.pace}
	return nil
}

// hashFrom is the hash of a content's bytes before the scrub's place
func (p *scrubbing) hashFrom(next scrubbed) (hash.Hash, error) {
	sum := sha256.New()
	if p.at.offset == 0 {
		return sum, nil
	}
	restorer, ok := sum.(encoding.BinaryUnmarshaler)
	if !ok {
		return nil, errors.New("blobs: SHA-256 keeps no state")
	}
	if err := restorer.UnmarshalBinary(p.at.state); err != nil {
		p.at = scrubPlace{content: next.id, pace: p.at.pace}
		return sha256.New(), nil //nolint:nilerr // a state that no longer reads begins its content again
	}
	return sum, nil
}

func stateOf(sum hash.Hash) ([]byte, error) {
	keeper, ok := sum.(encoding.BinaryMarshaler)
	if !ok {
		return nil, errors.New("blobs: SHA-256 keeps no state")
	}
	return keeper.MarshalBinary()
}

const bodyOf = `select bytes from bodies where id = ?1`

// readAt reads n bytes of a content at the scrub's place into the slice's
// buffer: an inline content's from its row, a file's from the file, opened
// for the read alone
func (p *scrubbing) readAt(ctx context.Context, next scrubbed, n int64) (int, error) {
	s, off := p.store, p.at.offset
	if next.inline {
		var body []byte
		err := s.file.Lookup(ctx, func(r sqlite.Reader) error {
			return sqlite.QueryRowByKey(ctx, r, bodyOf, next.id).Scan(&body)
		})
		if errors.Is(err, sql.ErrNoRows) || (err == nil && int64(len(body)) < off+n) {
			return 0, fs.ErrNotExist
		}
		return copy(p.buffer[:n], body[off:]), err
	}
	_, name := objectName(next.id)
	file, err := s.root.Open(name)
	if err != nil {
		return 0, err
	}
	read, err := file.ReadAt(p.buffer[:n], off)
	return read, errors.Join(err, file.Close())
}

const (
	markDamaged = `update contents set damaged = 1 where id = ?1 and names > 0`
	namesOf     = `select b.name, o.path from objects as o join buckets as b on b.id = o.bucket
		where o.content = ?1 limit 16`
)

// judge marks a content damaged, so that its next Open is ErrCorrupt, and logs
// it once at Error with the keys naming it; a content that lost its last name
// while it was read is left alone. A backup is what repairs it.
func (p *scrubbing) judge(ctx context.Context, next scrubbed, why string) error {
	s := p.store
	p.at = scrubPlace{content: next.id + 1, pace: p.at.pace}
	marked := false
	err := s.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		result, err := w.ExecContext(ctx, markDamaged, next.id)
		if err != nil {
			return err
		}
		affected, err := result.RowsAffected()
		marked = affected == 1
		return err
	})
	if err != nil || !marked {
		return err
	}
	p.damaged++
	keys, err := s.keysNaming(ctx, next.id)
	s.log.Error("a content's bytes are "+why, "content", next.id, "keys", keys)
	return err
}

// keysNaming is up to 16 keys that name a content, each its bucket and path;
// no index leads there, so it reads every object, which only damage asks for
func (s *Store) keysNaming(ctx context.Context, id int64) ([]string, error) {
	var keys []string
	err := s.file.Lookup(ctx, func(r sqlite.Reader) error {
		rows, err := r.QueryContext(ctx, namesOf, id) //nolint:rowserrcheck // EachRow checks Err
		if err != nil {
			return err
		}
		return sqlite.EachRow(rows, "the keys of a content", func(rows *sql.Rows) error {
			var bucket, path string
			err := rows.Scan(&bucket, &path)
			keys = append(keys, fmt.Sprintf("%s/%s", bucket, path))
			return err
		})
	})
	return keys, err
}
