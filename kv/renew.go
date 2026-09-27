package kv

import (
	"context"
	"database/sql"
	"sync"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// renewals are what reads of Sliding buckets asked for and the next flush
// writes: a read does not write, so a flood of reads costs no commits
type renewals struct {
	mu        sync.Mutex
	waiting   map[renewed]renewal
	scheduled sync.Once
}

type renewed struct {
	bucket int64
	path   string
}

// renewal is bound to the row its read saw, its version and its expiry, so
// that it cannot extend a key written again, touched or deleted since
type renewal struct {
	version int64
	seen    int64 // the expiry the read saw, unix milliseconds
	until   int64 // the expiry it asks for
}

// renew asks for a live key of a Sliding bucket, read at c.now, to live its
// term from now, once a thirtieth of the term has passed since it last did. A
// key with less than a minute left is renewed before the read returns, since
// the flush might come too late for it; the rest wait for the flush.
func (b *Bucket[V]) renew(ctx context.Context, c call, version int64, expires sql.NullInt64) {
	if b.sliding <= 0 || !expires.Valid {
		return
	}
	left := expires.Int64 - c.now
	if left >= (b.sliding - b.sliding/refreshes).Milliseconds() {
		return
	}

	due := renewal{version: version, seen: expires.Int64, until: c.now + b.sliding.Milliseconds()}
	if left < renewAtOnce.Milliseconds() && b.tx == nil && b.renewNow(ctx, c, due) == nil {
		return
	}
	b.state.renewals.ask(renewed{bucket: b.id, path: string(c.path)}, due)
}

const renewCell = `update cells set expires = ?5 where bucket = ?1 and path = ?2 and version = ?3 and expires = ?4`

// renewNow writes one renewal in a group of writes, as a Set is written
func (b *Bucket[V]) renewNow(ctx context.Context, c call, due renewal) error {
	return b.write(ctx, 0, func(w sqlite.Writer) error {
		_, err := w.ExecContext(ctx, renewCell, b.id, c.path, due.version, due.seen, due.until)
		return err
	})
}

// ask keeps a renewal for the next flush; past the bound it keeps nothing new,
// and the key's next read asks again
func (r *renewals) ask(key renewed, due renewal) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if _, asked := r.waiting[key]; !asked && len(r.waiting) >= maxRenewals {
		return
	}
	r.waiting[key] = due
}

// schedule has the store flush renewals every renewEvery, from the first
// Sliding bucket on
func (s *Store) scheduleRenewals() {
	s.renewals.scheduled.Do(func() {
		s.runtime.EveryEngine("kv", "kv renewals", renewEvery, func(ctx context.Context) error {
			_, err := s.flushRenewals(ctx)
			return err
		})
	})
}

// flushRenewals writes the renewals asked for, flushBatch a transaction, and
// returns how many keys they renewed; a key written, touched, deleted or
// cleared since its read keeps what was done to it
func (s *Store) flushRenewals(ctx context.Context) (int, error) {
	batch := s.renewals.take()
	renewedKeys := 0
	for len(batch) > 0 {
		size := min(len(batch), flushBatch)
		wrote, err := s.writeRenewals(ctx, batch[:size])
		if err != nil {
			s.renewals.giveBack(batch)
			return renewedKeys, err
		}
		renewedKeys += wrote
		batch = batch[size:]
	}
	return renewedKeys, nil
}

type askedRenewal struct {
	key renewed
	due renewal
}

func (r *renewals) take() []askedRenewal {
	r.mu.Lock()
	defer r.mu.Unlock()
	batch := make([]askedRenewal, 0, len(r.waiting))
	for key, due := range r.waiting {
		batch = append(batch, askedRenewal{key: key, due: due})
	}
	clear(r.waiting)
	return batch
}

// giveBack keeps for the next flush what a failed one took, unless a later
// read has asked for the key again
func (r *renewals) giveBack(batch []askedRenewal) {
	r.mu.Lock()
	defer r.mu.Unlock()
	for _, asked := range batch {
		if _, again := r.waiting[asked.key]; !again {
			r.waiting[asked.key] = asked.due
		}
	}
}

func (s *Store) writeRenewals(ctx context.Context, batch []askedRenewal) (int, error) {
	wrote := 0
	err := s.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		wrote = 0
		for _, asked := range batch {
			result, err := w.ExecContext(ctx, renewCell, asked.key.bucket, []byte(asked.key.path),
				asked.due.version, asked.due.seen, asked.due.until)
			if err != nil {
				return err
			}
			changed, err := result.RowsAffected()
			if err != nil {
				return err
			}
			wrote += int(changed)
		}
		return nil
	})
	return wrote, err
}
