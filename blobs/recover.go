package blobs

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"io/fs"
	"path/filepath"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

const selectMarks = `select (select value from meta where name = 'revision'),
	(select value from meta where name = 'ids'), (select value from meta where name = 'settled')`

// recover takes up the file's marks and removes what a process that died
// left: every file of uploads/, and in the directories of objects/ that hold
// the ids past the settled mark, each file whose id no content names. Only an
// upload makes such a file, between its rename and its commit, and every id
// at or below the mark is committed or has none, so nothing else is walked.
func (s *Store) recover(ctx context.Context) error {
	var revision, reserved, settled int64
	err := s.file.View(ctx, func(tx *sql.Tx) error {
		return tx.QueryRowContext(ctx, selectMarks).Scan(&revision, &reserved, &settled)
	})
	if err != nil {
		return fmt.Errorf("blobs: read the marks: %w", err)
	}
	s.revision.Store(revision)
	s.ids.last, s.ids.end = reserved, reserved

	if _, err = s.sweepUploads(); err != nil {
		return err
	}
	for fan := (settled + 1) >> idsPerDir; settled < reserved && fan <= reserved>>idsPerDir; fan++ {
		if err = s.removeUnnamedIn(ctx, fan, settled, reserved); err != nil {
			return err
		}
	}
	return s.settle(ctx)
}

// sweepUploads removes the files of uploads/ that no upload of this process
// holds: at open every one of them, and later what an upload failed to remove
func (s *Store) sweepUploads() (int, error) {
	names, err := s.namesIn(uploadsDir)
	if err != nil {
		return 0, err
	}
	removed := 0
	var failed []error
	for _, name := range names {
		if id, isID := idOf(name); isID && s.holds(id) {
			continue
		}
		if err = s.root.RemoveAll(filepath.Join(uploadsDir, name)); err != nil {
			failed = append(failed, err)
			continue
		}
		removed++
	}
	if err = errors.Join(failed...); err != nil {
		return removed, fmt.Errorf("blobs: empty %s: %w", uploadsDir, err)
	}
	return removed, nil
}

const namedBetween = `select id from contents where id >= ?1 and id <= ?2`

// removeUnnamedIn removes the files of one directory of objects/ whose ids
// lie past settled, up to reserved, and that no content names
func (s *Store) removeUnnamedIn(ctx context.Context, fan, settled, reserved int64) error {
	dir := objectDir(fan)
	names, err := s.namesIn(dir)
	if err != nil || len(names) == 0 {
		return err
	}
	low, high := max(settled+1, fan<<idsPerDir), min(reserved, (fan+1)<<idsPerDir-1)
	named := map[int64]bool{}
	err = s.file.Lookup(ctx, func(r sqlite.Reader) error {
		rows, queryErr := r.QueryContext(ctx, namedBetween, low, high) //nolint:rowserrcheck // EachRow checks Err
		if queryErr != nil {
			return queryErr
		}
		return sqlite.EachRow(rows, "contents named", func(rows *sql.Rows) error {
			var id int64
			scanErr := rows.Scan(&id)
			named[id] = true
			return scanErr
		})
	})
	for _, name := range names {
		if id, isID := idOf(name); err == nil && isID && id >= low && id <= high && !named[id] {
			err = s.removeFile(id)
		}
	}
	return err
}

// namesIn is the names a directory of blobs/ holds, none when it does not exist
func (s *Store) namesIn(dir string) ([]string, error) {
	entries, err := fs.ReadDir(s.root.FS(), filepath.ToSlash(dir))
	if errors.Is(err, fs.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("blobs: list %s: %w", dir, err)
	}
	names := make([]string, len(entries))
	for i, entry := range entries {
		names[i] = entry.Name()
	}
	return names, nil
}
