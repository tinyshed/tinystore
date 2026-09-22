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

// Migrate claims a fresh file or checks the existing engine's migration history.
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
		var owner int
		if err := tx.QueryRowContext(ctx, `pragma application_id`).Scan(&owner); err != nil {
			return fmt.Errorf("read application id: %w", err)
		}
		if owner != 0 && owner != applicationID {
			return fmt.Errorf("SQLite file belongs to application %d", owner)
		}
		if owner == 0 {
			var tables int
			if err := tx.QueryRowContext(ctx, `select count(*) from sqlite_schema where name not like 'sqlite_%'`).Scan(&tables); err != nil {
				return fmt.Errorf("inspect fresh file: %w", err)
			}
			if tables != 0 {
				return errors.New("refuse to claim a populated SQLite file")
			}
			if _, err := tx.ExecContext(ctx, fmt.Sprintf("pragma application_id=%d", applicationID)); err != nil {
				return fmt.Errorf("claim SQLite file: %w", err)
			}
			if _, err := tx.ExecContext(ctx, `create table _tinystore_migrations(version integer primary key,name text not null,checksum blob not null) strict`); err != nil {
				return fmt.Errorf("create migration history: %w", err)
			}
		}
		var applied int
		if err := tx.QueryRowContext(ctx, `select count(*) from _tinystore_migrations`).Scan(&applied); err != nil {
			return fmt.Errorf("read migration history: %w", err)
		}
		if applied > len(paths) {
			return errors.New("SQLite schema is newer than this reader")
		}
		for i, path := range paths {
			body, err := fs.ReadFile(scripts, path)
			if err != nil {
				return fmt.Errorf("read migration %s: %w", path, err)
			}
			sum := sha256.Sum256(body)
			if i < applied {
				var name string
				var stored []byte
				if err := tx.QueryRowContext(ctx, `select name,checksum from _tinystore_migrations where version=?`, i+1).Scan(&name, &stored); err != nil {
					return fmt.Errorf("read migration %d: %w", i+1, err)
				}
				if name != path || !bytes.Equal(stored, sum[:]) {
					return fmt.Errorf("migration %d changed after application", i+1)
				}
				continue
			}
			if _, err := tx.ExecContext(ctx, string(body)); err != nil {
				return fmt.Errorf("apply migration %s: %w", path, err)
			}
			if _, err := tx.ExecContext(ctx, `insert into _tinystore_migrations values(?,?,?)`, i+1, path, sum[:]); err != nil {
				return fmt.Errorf("record migration %s: %w", path, err)
			}
		}
		return nil
	})
}
