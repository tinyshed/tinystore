package sqlite

import (
	"context"
	"database/sql"
	"errors"
	"io/fs"
	"path/filepath"
	"strings"
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
	file, err := Open(ctx, path, Config{Readers: 2})
	if err != nil {
		t.Fatal(err)
	}
	if err = file.Migrate(ctx, 1234, scripts); err != nil {
		t.Fatal(err)
	}
	if err = file.Close(); err != nil {
		t.Fatal(err)
	}
	file, err = Open(ctx, path, Config{Readers: 2})
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
	file, err := Open(t.Context(), filepath.Join(t.TempDir(), "history.db"), Config{Readers: 1})
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

// a migration that rebuilds a parent keeps its children, since foreign keys
// are off while it runs; one that leaves a child referring to nothing is
// refused whole; and foreign keys are on again for the writes after them
func TestMigrationsRunWithoutForeignKeysAndCheckThemBeforeCommit(t *testing.T) {
	file, err := Open(t.Context(), filepath.Join(t.TempDir(), "keys.db"), Config{Readers: 1})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if closeErr := file.Close(); closeErr != nil {
			t.Error(closeErr)
		}
	})
	scripts := fstest.MapFS{"0001_schema.sql": {Data: []byte(`
		create table parents (id integer primary key) strict;
		create table children (id integer primary key,
			parent integer not null references parents (id) on delete cascade) strict;
		insert into parents values (1), (2);
		insert into children values (10, 1), (20, 2);`)}}
	if err = file.Migrate(t.Context(), 1234, scripts); err != nil {
		t.Fatal(err)
	}

	scripts["0002_rebuild.sql"] = &fstest.MapFile{Data: []byte(`
		create table parents_new (id integer primary key, name text not null default '') strict;
		insert into parents_new (id) select id from parents;
		drop table parents;
		alter table parents_new rename to parents;`)}
	if err = file.Migrate(t.Context(), 1234, scripts); err != nil {
		t.Fatal(err)
	}
	if children := countOf(t, file, `select count(*) from children`); children != 2 {
		t.Fatalf("%d children after their parent was rebuilt, want 2", children)
	}

	scripts["0003_orphan.sql"] = &fstest.MapFile{Data: []byte(`delete from parents where id = 2;`)}
	if err = file.Migrate(t.Context(), 1234, scripts); err == nil ||
		!strings.Contains(err.Error(), "children row 20 refers to no parents") {
		t.Fatalf("a migration leaving an orphan: %v", err)
	}
	if parents := countOf(t, file, `select count(*) from parents`); parents != 2 {
		t.Fatalf("the refused migration left %d parents, want 2", parents)
	}
	err = file.Update(t.Context(), func(tx *sql.Tx) error {
		_, insertErr := tx.ExecContext(t.Context(), `insert into children values (30, 99)`)
		return insertErr
	})
	if err == nil {
		t.Fatal("a write after the migrations ignored foreign keys")
	}
}

func countOf(t *testing.T, file *File, query string) int {
	t.Helper()
	var count int
	err := file.View(t.Context(), func(tx *sql.Tx) error { return tx.QueryRowContext(t.Context(), query).Scan(&count) })
	if err != nil {
		t.Fatal(err)
	}
	return count
}

// Verify checks a history and runs nothing: a script the file has not run is
// ErrPending, an edited one is refused, and a fresh file has run none
func TestVerifyChecksTheHistoryAndRunsNothing(t *testing.T) {
	file, err := Open(t.Context(), filepath.Join(t.TempDir(), "verified.db"), Config{Readers: 1})
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
	if err := file.Verify(t.Context(), 1234, fstest.MapFS{"0001_schema.sql": schema}); !errors.Is(err, ErrPending) {
		t.Fatalf("a fresh file: %v", err)
	}
	if err := file.Migrate(t.Context(), 1234, fstest.MapFS{"0001_schema.sql": schema}); err != nil {
		t.Fatal(err)
	}
	if err := file.Verify(t.Context(), 1234, fstest.MapFS{"0001_schema.sql": schema}); err != nil {
		t.Fatalf("the history as it is: %v", err)
	}
	both := fstest.MapFS{"0001_schema.sql": schema, "0002_labels.sql": labels}
	if err := file.Verify(t.Context(), 1234, both); !errors.Is(err, ErrPending) ||
		!strings.Contains(err.Error(), "0002_labels.sql") {
		t.Fatalf("a script the file has not run: %v", err)
	}
	if err := file.Verify(t.Context(), 1234, testMigrations(`create table other(n integer);`)); err == nil ||
		errors.Is(err, ErrPending) {
		t.Fatalf("an edited script: %v", err)
	}
	if err := file.Verify(t.Context(), 4321, both); err == nil || errors.Is(err, ErrPending) {
		t.Fatalf("another engine's file: %v", err)
	}
	if err := file.View(t.Context(), func(tx *sql.Tx) error {
		_, err := tx.ExecContext(t.Context(), `select n from labels`)
		return err
	}); err == nil {
		t.Fatal("Verify ran a script")
	}
}
