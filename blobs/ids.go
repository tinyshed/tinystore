package blobs

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"maps"
	"slices"
	"sync"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// ids hands out content ids from a block reserved in meta, so that an id is
// never given twice, not after a crash either. It holds each id until its
// upload ends, so that the settled mark stays below every id whose file may
// still lack its row.
//
// A doubtful id is one whose commit failed with its outcome unknown; it stays
// held until the file answers whether its content is there.
type ids struct {
	mu       sync.Mutex
	last     int64 // the last id handed out
	end      int64 // the last id of the block reserved
	held     map[int64]bool
	doubtful map[int64]bool
}

const (
	reserveIDs = `update meta set value = value + ?1 where name = 'ids' returning value`
	settleIDs  = `update meta set value = ?1 where name = 'settled' and value < ?1`
	revisionIs = `update meta set value = ?1 where name = 'revision'`
)

// takeID hands out the next id and holds it, reserving the next block in a
// transaction of its own when this one is spent
func (s *Store) takeID(ctx context.Context) (int64, error) {
	s.ids.mu.Lock()
	defer s.ids.mu.Unlock()
	if s.ids.last == s.ids.end {
		var end int64
		err := s.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
			return sqlite.QueryRowByKey(ctx, w, reserveIDs, idBlock).Scan(&end)
		})
		if err != nil {
			return 0, fmt.Errorf("blobs: reserve content ids: %w", err)
		}
		s.ids.last, s.ids.end = end-idBlock, end
	}
	s.ids.last++
	s.ids.held[s.ids.last] = true
	return s.ids.last, nil
}

// letGo ends an id's hold: its content committed, or its file is gone
func (s *Store) letGo(id int64) {
	s.ids.mu.Lock()
	defer s.ids.mu.Unlock()
	delete(s.ids.held, id)
	delete(s.ids.doubtful, id)
}

// doubt keeps an id held until maintenance learns what became of it
func (s *Store) doubt(id int64) {
	s.ids.mu.Lock()
	defer s.ids.mu.Unlock()
	s.ids.held[id] = true
	s.ids.doubtful[id] = true
}

// holds says whether an upload of this process still holds an id
func (s *Store) holds(id int64) bool {
	s.ids.mu.Lock()
	defer s.ids.mu.Unlock()
	return s.ids.held[id]
}

// settledMark is the highest id at or below which every id is committed or
// has no file: one below the first id held, else the last handed out
func (s *Store) settledMark() int64 {
	s.ids.mu.Lock()
	defer s.ids.mu.Unlock()
	mark := s.ids.last
	for id := range s.ids.held {
		mark = min(mark, id-1)
	}
	return mark
}

// settle moves the settled mark up to what no upload holds, so that the next
// open walks only the directories of the ids reserved since
func (s *Store) settle(ctx context.Context) error {
	mark := s.settledMark()
	err := s.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		_, err := w.ExecContext(ctx, settleIDs, mark)
		return err
	})
	if err != nil {
		return fmt.Errorf("blobs: settle content ids: %w", err)
	}
	return nil
}

const contentNamed = `select 1 from contents where id = ?1`

// resolve learns what became of a file content whose commit's outcome is
// unknown: a content row says it committed, and none that it did not, so its
// file goes. A file that cannot answer leaves the id doubtful.
func (s *Store) resolve(ctx context.Context, id int64) {
	var one int
	err := s.file.Lookup(ctx, func(r sqlite.Reader) error {
		return sqlite.QueryRowByKey(ctx, r, contentNamed, id).Scan(&one)
	})
	switch {
	case errors.Is(err, sql.ErrNoRows):
		err = s.removeFile(id)
	case err == nil:
		s.letGo(id)
		return
	}
	if err != nil {
		s.doubt(id)
		return
	}
	s.letGo(id)
}

// resolveDoubts asks the file again about every doubtful id
func (s *Store) resolveDoubts(ctx context.Context) {
	s.ids.mu.Lock()
	doubtful := slices.Collect(maps.Keys(s.ids.doubtful))
	s.ids.mu.Unlock()
	for _, id := range doubtful {
		s.resolve(ctx, id)
	}
}

// nextRevision is the revision of one write, kept as the file's high-water mark
// in the write's own transaction. A write rolled back leaves a gap, never a
// repeat.
func (s *Store) nextRevision(ctx context.Context, w sqlite.Writer) (int64, error) {
	revision := s.revision.Add(1)
	_, err := w.ExecContext(ctx, revisionIs, revision)
	return revision, err
}
