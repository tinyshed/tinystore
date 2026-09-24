package sqlite

import (
	"bytes"
	"context"
	"crypto/sha256"
	"database/sql"
	"errors"
	"fmt"
	"io/fs"
	"sort"
)

// Migrate claims a fresh file or checks the existing engine's migration
// history, then runs, in name order, every script the file has not run yet:
//
//	history   0001_schema.sql
//	scripts   0001_schema.sql   0002_labels.sql
//	          checked           run and recorded
func (f *File) Migrate(ctx context.Context, applicationID int, scripts fs.FS) error {
	paths, err := fs.Glob(scripts, "*.sql")
	if err != nil {
		return fmt.Errorf("list migrations: %w", err)
	}
	if len(paths) == 0 || applicationID <= 0 {
		return errors.New("migrate SQLite: missing scripts or application id")
	}
	sort.Strings(paths)

	return f.Update(ctx, func(tx *sql.Tx) error {
		if err := claimFile(ctx, tx, applicationID); err != nil {
			return err
		}

		applied, err := countApplied(ctx, tx, len(paths))
		if err != nil {
			return err
		}

		for i, path := range paths {
			script, err := readMigration(scripts, path, i+1)
			if err != nil {
				return err
			}
			if i < applied {
				err = script.verify(ctx, tx)
			} else {
				err = script.apply(ctx, tx)
			}
			if err != nil {
				return err
			}
		}
		return nil
	})
}

const (
	applicationIDQuery = `pragma application_id`
	claimQuery         = `pragma application_id=%d`
	userTablesQuery    = `select count(*) from sqlite_schema where name not like 'sqlite_%'`

	// the file's schema keeps this text, so it stays as the first files wrote it
	historyTableQuery = `create table _tinystore_migrations(` +
		`version integer primary key,name text not null,checksum blob not null) strict`
)

// claimFile stamps a fresh, empty file with the engine's application id and a
// migration history, and refuses a file that belongs to another engine.
func claimFile(ctx context.Context, tx *sql.Tx, applicationID int) error {
	var owner int
	if err := tx.QueryRowContext(ctx, applicationIDQuery).Scan(&owner); err != nil {
		return fmt.Errorf("read application id: %w", err)
	}
	if owner != 0 && owner != applicationID {
		return fmt.Errorf("SQLite file belongs to application %d", owner)
	}
	if owner != 0 {
		return nil
	}

	var tables int
	if err := tx.QueryRowContext(ctx, userTablesQuery).Scan(&tables); err != nil {
		return fmt.Errorf("inspect fresh file: %w", err)
	}
	if tables != 0 {
		return errors.New("refuse to claim a populated SQLite file")
	}
	if _, err := tx.ExecContext(ctx, fmt.Sprintf(claimQuery, applicationID)); err != nil {
		return fmt.Errorf("claim SQLite file: %w", err)
	}
	if _, err := tx.ExecContext(ctx, historyTableQuery); err != nil {
		return fmt.Errorf("create migration history: %w", err)
	}
	return nil
}

const appliedCountQuery = `select count(*) from _tinystore_migrations`

// countApplied refuses a file that has run more scripts than this binary knows.
func countApplied(ctx context.Context, tx *sql.Tx, known int) (int, error) {
	var applied int
	if err := tx.QueryRowContext(ctx, appliedCountQuery).Scan(&applied); err != nil {
		return 0, fmt.Errorf("read migration history: %w", err)
	}
	if applied > known {
		return 0, errors.New("SQLite schema is newer than this reader")
	}
	return applied, nil
}

// migration is one script at its place in the history.
type migration struct {
	version  int
	path     string
	body     []byte
	checksum [sha256.Size]byte
}

func readMigration(scripts fs.FS, path string, version int) (migration, error) {
	body, err := fs.ReadFile(scripts, path)
	if err != nil {
		return migration{}, fmt.Errorf("read migration %s: %w", path, err)
	}
	return migration{version: version, path: path, body: body, checksum: sha256.Sum256(body)}, nil
}

const appliedMigrationQuery = `select name,checksum from _tinystore_migrations where version=?`

// verify refuses a script renamed or edited after the file ran it.
func (m migration) verify(ctx context.Context, tx *sql.Tx) error {
	var name string
	var stored []byte
	if err := tx.QueryRowContext(ctx, appliedMigrationQuery, m.version).Scan(&name, &stored); err != nil {
		return fmt.Errorf("read migration %d: %w", m.version, err)
	}
	if name != m.path || !bytes.Equal(stored, m.checksum[:]) {
		return fmt.Errorf("migration %d changed after application", m.version)
	}
	return nil
}

const recordMigrationQuery = `insert into _tinystore_migrations values(?,?,?)`

func (m migration) apply(ctx context.Context, tx *sql.Tx) error {
	if _, err := tx.ExecContext(ctx, string(m.body)); err != nil {
		return fmt.Errorf("apply migration %s: %w", m.path, err)
	}
	if _, err := tx.ExecContext(ctx, recordMigrationQuery, m.version, m.path, m.checksum[:]); err != nil {
		return fmt.Errorf("record migration %s: %w", m.path, err)
	}
	return nil
}
