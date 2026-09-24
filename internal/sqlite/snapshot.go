package sqlite

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
)

const (
	snapshotQuery = `vacuum into ?`
	appliedQuery  = `select count(*) from _tinystore_migrations`
)

// Snapshot writes a consistent copy of the file to into, which must not exist,
// and returns how many migrations the copy has applied. It runs on a
// connection of its own, opened read-only at the file: the query_only readers
// refuse VACUUM INTO, and a read-only file lets the writer keep writing.
func (f *File) Snapshot(ctx context.Context, into string) (applied int, err error) {
	if err = os.MkdirAll(filepath.Dir(into), 0o750); err != nil {
		return 0, fmt.Errorf("snapshot SQLite: %w", err)
	}
	copier, err := sql.Open("sqlite", connectionURL(f.path, snapshotArguments()))
	if err != nil {
		return 0, fmt.Errorf("snapshot SQLite: %w", err)
	}
	defer func() { err = errors.Join(err, copier.Close()) }()

	if err = copier.QueryRowContext(ctx, appliedQuery).Scan(&applied); err != nil {
		return 0, fmt.Errorf("snapshot SQLite: migration history: %w", err)
	}
	if _, err = copier.ExecContext(ctx, snapshotQuery, into); err != nil {
		return 0, fmt.Errorf("snapshot SQLite into %s: %w", into, err)
	}
	return applied, nil
}

// snapshotArguments open the file read-only, and wait for the writer's
// checkpoint rather than fail on it
func snapshotArguments() url.Values {
	arguments := url.Values{"mode": {"ro"}}
	arguments.Add("_pragma", "busy_timeout(5000)")
	return arguments
}
