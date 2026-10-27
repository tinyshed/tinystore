package blobs_test

import (
	"bytes"
	"context"
	"errors"
	"testing"
	"testing/fstest"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/blobs"
	"tinyshed.org/tinystore/internal/ways"
	"tinyshed.org/tinystore/sqldb"
)

// contract: DUR-06
func TestFilesOpenedFromADatabaseCommitWithItsRows(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		app := ways.Must(sqldb.Open(ctx, store, "app", sqldb.Migrations(fstest.MapFS{
			"0001_plugins.sql": {Data: []byte("create table plugins (file text primary key) strict")},
		})))
		packages := ways.Must(blobs.OpenFiles(app, "packages"))
		pkg := bytes.Repeat([]byte{7}, 40<<10)
		declined := errors.New("declined")
		err := app.Tx(ctx, func(tx *sqldb.Tx) error {
			if _, err := tx.Exec(ctx, "insert into plugins (file) values ('stripe.js')"); err != nil {
				return err
			}
			if _, err := packages.In(tx).Put(ctx, "stripe.js", bytes.NewReader(pkg)); err != nil {
				return err
			}
			if _, found, err := packages.In(tx).Head(ctx, "stripe.js"); err != nil || !found {
				t.Fatalf("a head inside sees the transaction's own file: %v %v", found, err)
			}
			return declined
		})
		if !errors.Is(err, declined) {
			t.Fatalf("tx %v", err)
		}
		if _, found := ways.Must2(packages.Head(ctx, "stripe.js")); found {
			t.Fatal("a rolled back file is not there")
		}
		ways.Must(0, app.Tx(ctx, func(tx *sqldb.Tx) error {
			if _, err := tx.Exec(ctx, "insert into plugins (file) values ('stripe.js')"); err != nil {
				return err
			}
			_, err := packages.In(tx).Put(ctx, "stripe.js", bytes.NewReader(pkg))
			return err
		}))
		info, found := ways.Must2(packages.Head(ctx, "stripe.js"))
		if !found || info.Size != int64(len(pkg)) {
			t.Fatalf("committed with its row: %v %v", info, found)
		}
		err = app.Tx(ctx, func(tx *sqldb.Tx) error {
			_, err := packages.In(tx).Upload(ctx, "big.js")
			return err
		})
		if !errors.Is(err, tinystore.ErrInvalid) {
			t.Fatalf("an upload inside a transaction: %v", err)
		}
	})
}
