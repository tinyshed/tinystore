// Command search links one SQL database with FTS5 and R*Tree, so task size can report what they cost.
package main

import (
	"context"
	"fmt"
	"os"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/sqldb"
	_ "github.com/tinyshed/tinystore/sqldb/fts5"
	_ "github.com/tinyshed/tinystore/sqldb/rtree"
)

func main() {
	fmt.Println(open(context.Background()))
}

func open(ctx context.Context) error {
	directory, err := os.MkdirTemp("", "tinystore-size-")
	if err != nil {
		return err
	}
	defer func() { _ = os.RemoveAll(directory) }()
	store, err := tinystore.Open(ctx, directory, tinystore.Options{})
	if err != nil {
		return err
	}
	defer func() { _ = store.Close(ctx) }()
	db, err := sqldb.Open(ctx, store, "app", nil, nil)
	if err != nil {
		return err
	}
	_, err = db.Exec(ctx, `create table notes (id integer primary key, title text not null) strict`)
	return err
}
