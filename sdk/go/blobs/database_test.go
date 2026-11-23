package blobs_test

import (
	"bytes"
	"context"
	"errors"
	"slices"
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

// contract: BLOB-25
func TestAFolderIsListedAndClearedInsideATransactionAsTheTransactionLeftIt(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		app := ways.Must(sqldb.Open(ctx, store, "app", sqldb.Migrations(fstest.MapFS{
			"0001_plugins.sql": {Data: []byte("create table plugins (file text primary key) strict")},
		})))
		packs := ways.Must(blobs.OpenFiles(app, "packs"))
		paths := func(files *blobs.Files) []string {
			found := []string{}
			for _, file := range ways.Must(files.List(ctx)).Files {
				found = append(found, file.Path)
			}
			return found
		}
		svg := func() *bytes.Reader { return bytes.NewReader([]byte("<svg/>")) }
		ways.Must(0, app.Tx(ctx, func(tx *sqldb.Tx) error {
			brands := packs.Folder("brands").In(tx)
			if _, err := brands.Put(ctx, "icons/youtube.svg", svg()); err != nil {
				return err
			}
			_, err := brands.Put(ctx, "icons/vimeo.svg", svg())
			return err
		}))
		ways.Must(0, app.Tx(ctx, func(tx *sqldb.Tx) error {
			brands := packs.Folder("brands").In(tx)
			if err := brands.Clear(ctx); err != nil {
				return err
			}
			if left := paths(brands); len(left) != 0 {
				t.Fatalf("a list inside sees the clear: %v", left)
			}
			_, err := brands.Put(ctx, "icons/youtube.svg", svg())
			return err
		}))
		if left := paths(packs); !slices.Equal(left, []string{"brands/icons/youtube.svg"}) {
			t.Fatalf("the folder replaced whole: %v", left)
		}
		declined := errors.New("declined")
		err := app.Tx(ctx, func(tx *sqldb.Tx) error {
			if err := packs.In(tx).Clear(ctx); err != nil {
				return err
			}
			return declined
		})
		if !errors.Is(err, declined) {
			t.Fatalf("tx %v", err)
		}
		if left := paths(packs); !slices.Equal(left, []string{"brands/icons/youtube.svg"}) {
			t.Fatalf("a clear rolls back: %v", left)
		}
	})
}
