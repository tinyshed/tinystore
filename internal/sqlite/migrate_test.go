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

func TestMigrateRunsOnlyWhatTheFileHasNotRun(t *testing.T) {
	file, err := Open(t.Context(), filepath.Join(t.TempDir(), "history.db"), 1)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := file.Close(); err != nil {
			t.Error(err)
		}
	})
	schema := &fstest.MapFile{Data: []byte(`create table series(n integer) strict;`)}
	labels := &fstest.MapFile{Data: []byte(`create table labels(n integer) strict;`)}
	if err := file.Migrate(t.Context(), 1234, fstest.MapFS{"0001_schema.sql": schema}); err != nil {
		t.Fatal(err)
	}
	// running 0001 again would fail on its existing table, so success means it was only checked
	scripts := fstest.MapFS{"0001_schema.sql": schema, "0002_labels.sql": labels}
	if err := file.Migrate(t.Context(), 1234, scripts); err != nil {
		t.Fatal(err)
	}
	if err := file.View(t.Context(), func(tx *sql.Tx) error {
		var history string
		if err := tx.QueryRowContext(t.Context(), `select group_concat(name, ' ') from _tinystore_migrations`).Scan(&history); err != nil {
			return err
		}
		if history != "0001_schema.sql 0002_labels.sql" {
			t.Errorf("history %q", history)
		}
		_, err := tx.ExecContext(t.Context(), `select n from labels`)
		return err
	}); err != nil {
		t.Fatal(err)
	}
}
