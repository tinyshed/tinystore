package sqlite

import (
	"context"
	"database/sql"
	"net/url"
	"path/filepath"
	"testing"
)

func TestConnectionURLCarriesEveryPragma(t *testing.T) {
	const (
		pragmas = `_pragma=foreign_keys%281%29&_pragma=busy_timeout%285000%29` +
			`&_pragma=synchronous%28FULL%29&_pragma=cache_size%28-1024%29`
		writer = `&_txlock=immediate&mode=rwc`
		reader = `&_pragma=query_only%281%29&_txlock=deferred&mode=rw`
	)
	for _, test := range []struct {
		path      string
		arguments url.Values
		want      string
	}{
		{"/data/metrics.db", writerArguments(), `file:///data/metrics.db?` + pragmas + writer},
		{"/data/metrics.db", readerArguments(), `file:///data/metrics.db?` + pragmas + reader},
		{"/tmp/a # b & c.db", writerArguments(), `file:///tmp/a%20%23%20b%20&%20c.db?` + pragmas + writer},
	} {
		if got := connectionURL(test.path, test.arguments); got != test.want {
			t.Errorf("%s\n got %s\nwant %s", test.path, got, test.want)
		}
	}
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
