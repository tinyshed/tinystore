// Command sql links one SQL database and nothing else, so task size can report what it costs.
package main

import (
	"context"
	"fmt"
	"os"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/sqldb"
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
