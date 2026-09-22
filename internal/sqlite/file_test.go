package sqlite

import (
	"context"
	"database/sql"
	"io/fs"
	"path/filepath"
	"testing"
	"testing/fstest"
)

func testMigrations(text string) fs.FS {
	return fstest.MapFS{"0001_table.sql": &fstest.MapFile{Data: []byte(text)}}
}

func TestFileEnforcesReadOnlyAndTransactionalWrites(t *testing.T) {
	ctx := context.Background()
	file, err := Open(ctx, filepath.Join(t.TempDir(), "a # b & c.db"), 2)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	if err = file.Migrate(ctx, 1234, testMigrations(`create table example(n integer) strict; insert into example values(1);`)); err != nil {
		t.Fatal(err)
	}
	if err = file.View(ctx, func(tx *sql.Tx) error {
		_, writeErr := tx.ExecContext(ctx, `insert into example values(2)`)
		return writeErr
	}); err == nil {
		t.Fatal("reader wrote")
	}
	if err = file.Update(ctx, func(tx *sql.Tx) error {
		_, writeErr := tx.ExecContext(ctx, `insert into example values(3)`)
		return writeErr
	}); err != nil {
		t.Fatal(err)
	}
	if err = file.View(ctx, func(tx *sql.Tx) error {
		var n int
		readErr := tx.QueryRowContext(ctx, `select sum(n) from example`).Scan(&n)
		if n != 4 {
			t.Errorf("sum %d", n)
		}
		return readErr
	}); err != nil {
		t.Fatal(err)
	}
}

func TestMigrationHistoryIsVerifiedOnReopen(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "schema.db")
	scripts := testMigrations(`create table example(n integer) strict;`)
	file, err := Open(ctx, path, 2)
	if err != nil {
		t.Fatal(err)
	}
	if err = file.Migrate(ctx, 1234, scripts); err != nil {
		t.Fatal(err)
	}
	if err = file.Close(); err != nil {
		t.Fatal(err)
	}
	file, err = Open(ctx, path, 2)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	if err = file.Migrate(ctx, 1234, scripts); err != nil {
		t.Fatal(err)
	}
	if err = file.Migrate(ctx, 1234, testMigrations(`create table changed(n integer);`)); err == nil {
		t.Fatal("changed migration accepted")
	}
	if err = file.Migrate(ctx, 4321, scripts); err == nil {
		t.Fatal("foreign owner accepted")
	}
}
