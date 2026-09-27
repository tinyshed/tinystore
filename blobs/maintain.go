package blobs

import (
	"context"
	"database/sql"
	"errors"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Maintenance is what one Maintain call did.
type Maintenance struct {
	Expired  int   // objects past their expiry, removed
	Cleared  int   // objects a marked Clear hid, removed
	Removed  int   // files of contents no key names, removed
	Aborted  int   // uploads whose context ended while no call held them
	Scrubbed int64 // bytes the scrub read
	Damaged  int   // contents the scrub found changed or missing
}

// Maintain removes expired objects and the objects marked Clears hid, 10,000
// a transaction and at most ten transactions of each a call; removes the
// files no key names any more; aborts the uploads whose context has ended and
// removes what uploads left behind; moves the settled mark up; and scrubs a
// slice of the contents. The store calls it every minute unless it is Manual.
func (s *Store) Maintain(ctx context.Context) (Maintenance, error) {
	release, err := s.holdMaintenance(ctx)
	if err != nil {
		return Maintenance{}, err
	}
	defer release()
	leave, err := s.admit(ctx)
	if err != nil {
		return Maintenance{}, err
	}
	defer leave()

	var done Maintenance
	var errs [6]error
	done.Expired, errs[0] = s.expire(ctx)
	done.Cleared, errs[1] = s.dropCleared(ctx)
	done.Removed, errs[2] = s.removeUnnamed(ctx)
	done.Aborted = s.live.abortEnded()
	_, errs[3] = s.sweepUploads()
	s.resolveDoubts(ctx)
	errs[4] = s.settle(ctx)
	done.Scrubbed, done.Damaged, errs[5] = s.scrub(ctx)
	return done, errors.Join(errs[:]...)
}

func (s *Store) maintainInBackground(ctx context.Context) error {
	_, err := s.Maintain(ctx)
	return err
}

// holdMaintenance lets one Maintain at a time run on a store
func (s *Store) holdMaintenance(ctx context.Context) (release func(), err error) {
	select {
	case <-s.maintenance:
		return func() { s.maintenance <- struct{}{} }, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

// the oldest expired objects first, through the expiry index
const expireObjects = `delete from objects where (bucket, path) in (
		select bucket, path from objects where expires <= ?1 order by expires limit cast(?2 as integer)
	) returning content`

func (s *Store) expire(ctx context.Context) (int, error) {
	now := s.clock()
	return s.batches(ctx, func(ch *change) ([]int64, error) {
		return ch.deleted(expireObjects, now, maintainBatch)
	})
}

// batches runs batch in a transaction of its own until one deletes fewer than
// a full batch, at most ten times: each takes a name from the contents of the
// objects it deleted, and removes the files it leaves without names once it
// has committed
func (s *Store) batches(ctx context.Context, batch func(*change) ([]int64, error)) (int, error) {
	total := 0
	for range maintainBatches {
		deleted := 0
		var freed []int64
		err := s.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
			ch := &change{ctx: ctx, w: w, store: s, now: s.clock()}
			contents, err := batch(ch)
			if err == nil {
				err = ch.releaseAll(contents)
			}
			deleted, freed = len(contents), ch.freed
			return err
		})
		if err != nil {
			return total, err
		}
		total += deleted
		s.letFilesGo(freed)
		if deleted < maintainBatch {
			return total, nil
		}
	}
	return total, nil
}

// mark is one marked Clear: under prefix, objects of revision cleared or
// older are gone
type mark struct {
	bucket   int64
	prefix   string
	revision int64
}

const (
	selectCleared = `select bucket, prefix, revision from cleared`
	dropHidden    = `delete from objects where (bucket, path) in (
		select bucket, path from objects where bucket = ?1 and path >= ?2 and path < ?3 and revision <= ?4
		limit cast(?5 as integer)
	) returning content`
	unmark = `delete from cleared where bucket = ?1 and prefix = ?2 and revision = ?3`
)

// dropCleared deletes the objects marked Clears hid, a batch a transaction and
// at most ten transactions a mark, and each mark with the last of its objects
func (s *Store) dropCleared(ctx context.Context) (int, error) {
	marks, err := s.readMarks(ctx)
	total := 0
	for _, marked := range marks {
		if err != nil {
			break
		}
		var dropped int
		dropped, err = s.batches(ctx, func(ch *change) ([]int64, error) {
			contents, dropErr := ch.deleted(dropHidden, marked.bucket, marked.prefix, prefixEnd(marked.prefix),
				marked.revision, maintainBatch)
			if dropErr == nil && len(contents) < maintainBatch {
				_, dropErr = ch.w.ExecContext(ch.ctx, unmark, marked.bucket, marked.prefix, marked.revision)
			}
			return contents, dropErr
		})
		total += dropped
	}
	return total, err
}

func (s *Store) readMarks(ctx context.Context) ([]mark, error) {
	var marks []mark
	err := s.file.Lookup(ctx, func(r sqlite.Reader) error {
		rows, err := r.QueryContext(ctx, selectCleared) //nolint:rowserrcheck // EachRow checks Err
		if err != nil {
			return err
		}
		return sqlite.EachRow(rows, "the marks of Clears", func(rows *sql.Rows) error {
			var marked mark
			if err := rows.Scan(&marked.bucket, &marked.prefix, &marked.revision); err != nil {
				return err
			}
			marks = append(marks, marked)
			return nil
		})
	})
	return marks, err
}

const (
	selectUnnamed = `select id from contents where names = 0 order by id limit cast(?1 as integer)`
	dropUnnamed   = `delete from contents where id = ?1 and names = 0`
)

// removeUnnamed removes the files of contents no key names, which a commit
// left behind because its own removal failed or a snapshot was linking files,
// then their rows, a batch at a time; a file that will not go stays listed,
// and is tried again next time
func (s *Store) removeUnnamed(ctx context.Context) (int, error) {
	if !s.collection.TryRLock() {
		return 0, nil
	}
	defer s.collection.RUnlock()
	removed := 0
	for range maintainBatches {
		unnamed, err := s.unnamed(ctx)
		if err != nil || len(unnamed) == 0 {
			return removed, err
		}
		gone := s.removeFiles(unnamed)
		if err = s.forget(ctx, gone); err != nil {
			return removed, err
		}
		removed += len(gone)
		if len(unnamed) < maintainBatch || len(gone) < len(unnamed) {
			return removed, nil
		}
	}
	return removed, nil
}

func (s *Store) unnamed(ctx context.Context) ([]int64, error) {
	var ids []int64
	err := s.file.Lookup(ctx, func(r sqlite.Reader) error {
		rows, err := r.QueryContext(ctx, selectUnnamed, maintainBatch) //nolint:rowserrcheck // EachRow checks Err
		if err != nil {
			return err
		}
		return sqlite.EachRow(rows, "contents no key names", func(rows *sql.Rows) error {
			var id int64
			err := rows.Scan(&id)
			ids = append(ids, id)
			return err
		})
	})
	return ids, err
}

// removeFiles removes each file it can and returns whose went
func (s *Store) removeFiles(ids []int64) []int64 {
	gone := make([]int64, 0, len(ids))
	for _, id := range ids {
		if err := s.removeFile(id); err != nil {
			s.removals.observe(s.now(), err)
			continue
		}
		gone = append(gone, id)
	}
	return gone
}

// forget deletes the rows of contents whose files are gone
func (s *Store) forget(ctx context.Context, gone []int64) error {
	if len(gone) == 0 {
		return nil
	}
	return s.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		for _, id := range gone {
			if _, err := w.ExecContext(ctx, dropUnnamed, id); err != nil {
				return err
			}
		}
		return nil
	})
}
