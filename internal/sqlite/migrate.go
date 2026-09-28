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
	"strings"
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

	return f.update(ctx, func(connection *writeConnection) (bool, error) {
		return withoutForeignKeys(ctx, connection.conn, func(tx *sql.Tx) error {
			return runMigrations(ctx, tx, applicationID, scripts, paths)
		})
	})
}

// ErrPending is a script the file has not run, found by Verify, which runs
// none.
var ErrPending = errors.New("migrations the file has not applied")

// ErrMismatch is a history the scripts given do not match: a script changed
// or renamed after the file ran it, fewer scripts than the file ran, or
// another engine's file.
var ErrMismatch = errors.New("migrations that do not match the file")

// Verify checks the scripts against the history the file ran, in a snapshot,
// and runs none: a script changed or renamed after the file ran it, or a
// history longer than the scripts, is refused as Migrate refuses it, and a
// script the file has not run, or a file no engine has claimed, is ErrPending.
//
//	history   0001_schema.sql
//	scripts   0001_schema.sql   0002_labels.sql
//	          checked           ErrPending
func (f *File) Verify(ctx context.Context, applicationID int, scripts fs.FS) error {
	paths, err := fs.Glob(scripts, "*.sql")
	if err != nil {
		return fmt.Errorf("list migrations: %w", err)
	}
	if len(paths) == 0 || applicationID <= 0 {
		return errors.New("verify SQLite: missing scripts or application id")
	}
	sort.Strings(paths)

	return f.View(ctx, func(tx *sql.Tx) error {
		return verifyMigrations(ctx, tx, applicationID, scripts, paths)
	})
}

func verifyMigrations(ctx context.Context, tx *sql.Tx, applicationID int, scripts fs.FS, paths []string) error {
	var owner int
	if err := tx.QueryRowContext(ctx, applicationIDQuery).Scan(&owner); err != nil {
		return fmt.Errorf("read application id: %w", err)
	}
	switch {
	case owner == 0:
		return fmt.Errorf("%w: %d, and the file none", ErrPending, len(paths))
	case owner != applicationID:
		return fmt.Errorf("%w: SQLite file belongs to application %d", ErrMismatch, owner)
	}

	applied, err := countApplied(ctx, tx, len(paths))
	if err != nil {
		return err
	}
	for i, path := range paths[:applied] {
		script, err := readMigration(scripts, path, i+1)
		if err == nil {
			err = script.verify(ctx, tx)
		}
		if err != nil {
			return err
		}
	}
	if applied < len(paths) {
		return fmt.Errorf("%w: %s and %d after it", ErrPending, paths[applied], len(paths)-applied-1)
	}
	return nil
}

// runMigrations checks the scripts the file has run and runs the rest, then
// refuses a history that leaves a row referring to nothing
func runMigrations(ctx context.Context, tx *sql.Tx, applicationID int, scripts fs.FS, paths []string) error {
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
	if applied == len(paths) {
		return nil
	}
	return checkForeignKeys(ctx, tx)
}

const (
	foreignKeysOff  = `pragma foreign_keys = off`
	foreignKeysOn   = `pragma foreign_keys = on`
	foreignKeyCheck = `pragma foreign_key_check`
)

// withoutForeignKeys runs work in a transaction with foreign keys off, as
// SQLite's procedure for changing a table asks: with them on, dropping a
// parent to rebuild it deletes its children through their ON DELETE actions.
// foreign_keys cannot change inside a transaction, so it goes off before BEGIN
// and on after COMMIT
func withoutForeignKeys(ctx context.Context, conn *sql.Conn, work func(*sql.Tx) error) (bool, error) {
	if _, err := conn.ExecContext(ctx, foreignKeysOff); err != nil {
		return false, fmt.Errorf("turn foreign keys off: %w", err)
	}
	reusable, err := transactReusable(ctx, conn, work)
	if _, onErr := conn.ExecContext(context.WithoutCancel(ctx), foreignKeysOn); onErr != nil {
		return false, errors.Join(err, fmt.Errorf("turn foreign keys on: %w", onErr))
	}
	return reusable, err
}

// checkForeignKeys refuses migrations that leave a row whose reference finds
// no parent, naming the first few
func checkForeignKeys(ctx context.Context, tx *sql.Tx) error {
	rows, err := tx.QueryContext(ctx, foreignKeyCheck)
	if err != nil {
		return fmt.Errorf("check foreign keys: %w", err)
	}
	var named []string
	broken := 0
	err = EachRow(rows, "foreign key check", func(rows *sql.Rows) error {
		var table, parent string
		var row, key sql.NullInt64
		if scanErr := rows.Scan(&table, &row, &parent, &key); scanErr != nil {
			return scanErr
		}
		if broken++; len(named) < 3 {
			named = append(named, fmt.Sprintf("%s row %d refers to no %s", table, row.Int64, parent))
		}
		return nil
	})
	if err == nil && broken > 0 {
		err = fmt.Errorf("migrations leave %d rows referring to nothing: %s", broken, strings.Join(named, "; "))
	}
	return err
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
		return fmt.Errorf("%w: SQLite file belongs to application %d", ErrMismatch, owner)
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
		return 0, fmt.Errorf("%w: SQLite schema is newer than this reader", ErrMismatch)
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
		return fmt.Errorf("%w: migration %d changed after application", ErrMismatch, m.version)
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
