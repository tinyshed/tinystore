package blobs

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path"
	"path/filepath"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

const filesNamed = `select id from contents where not inline and names > 0 order by id`

// Snapshot copies blobs.db into dir while the engine keeps working, and links
// there every file the copy names. A link copies no byte and keeps the bytes
// after the engine removes its own name, so a 17 GB film costs the snapshot a
// directory entry.
//
// Collection waits meanwhile, so that no file the copy names goes before it is
// linked. A file missing is left out and logged, and one the system will not
// link is copied.
func (s *Store) Snapshot(ctx context.Context, dir string) ([]tinystore.SnapshotFile, error) {
	s.collection.Lock()
	defer s.collection.Unlock()

	copied := path.Join(dirName, fileName)
	schema, err := s.file.Snapshot(ctx, tinystore.SnapshotPath(dir, copied))
	if err != nil {
		return nil, fmt.Errorf("snapshot blobs: %w", err)
	}
	ids, err := namedFiles(ctx, tinystore.SnapshotPath(dir, copied))
	if err != nil {
		return nil, fmt.Errorf("snapshot blobs: %w", err)
	}

	files := []tinystore.SnapshotFile{{Name: copied, Engine: "blobs", Schema: schema}}
	linker := linker{store: s, dir: dir, made: map[string]bool{}}
	for _, id := range ids {
		name, linked, err := linker.link(ctx, id)
		if err != nil {
			return nil, fmt.Errorf("snapshot blobs: %w", err)
		}
		if linked {
			files = append(files, tinystore.SnapshotFile{Name: name, Engine: "blobs", Schema: schema, Stored: true})
		}
	}
	return files, nil
}

// namedFiles is the ids of the files a copy of blobs.db names, read from the copy
func namedFiles(ctx context.Context, copied string) ([]int64, error) {
	var ids []int64
	err := sqlite.ReadCopy(ctx, copied, func(r sqlite.Reader) error {
		rows, err := r.QueryContext(ctx, filesNamed) //nolint:rowserrcheck // EachRow checks Err
		if err != nil {
			return err
		}
		return sqlite.EachRow(rows, "the files a copy names", func(rows *sql.Rows) error {
			var id int64
			err := rows.Scan(&id)
			ids = append(ids, id)
			return err
		})
	})
	return ids, err
}

// linker puts the files of a snapshot beside its copy of blobs.db, creating
// each directory once
type linker struct {
	store *Store
	dir   string
	made  map[string]bool
}

// link puts a content's file into the snapshot under its own name: a hard link,
// or a copy where the system links nothing. A file missing is left out and
// logged, and the restored store reports it as this one does.
func (l *linker) link(ctx context.Context, id int64) (name string, linked bool, err error) {
	if err = ctx.Err(); err != nil {
		return "", false, err
	}
	_, own := objectName(id)
	name = path.Join(dirName, filepath.ToSlash(own))
	target := tinystore.SnapshotPath(l.dir, name)
	if parent := filepath.Dir(target); !l.made[parent] {
		if err = os.MkdirAll(parent, 0o750); err != nil {
			return "", false, err
		}
		l.made[parent] = true
	}
	err = os.Link(filepath.Join(l.store.dir, own), target)
	switch {
	case err == nil:
		return name, true, nil
	case errors.Is(err, fs.ErrNotExist):
		l.store.log.Warn("a file the snapshot names is missing", "content", id)
		return "", false, nil
	}
	if err = l.copy(own, target); err != nil {
		return "", false, err
	}
	return name, true, nil
}

// copy writes a file's bytes to the snapshot, for a system that will not link it
func (l *linker) copy(own, target string) (err error) {
	source, err := l.store.root.Open(own)
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, source.Close()) }()
	copied, err := os.OpenFile(target, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o600) //nolint:gosec // inside the snapshot
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, copied.Close()) }()
	_, err = io.Copy(copied, source)
	return err
}
