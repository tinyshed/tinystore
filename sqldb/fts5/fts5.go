// Package fts5 gives every sqldb database SQLite's FTS5, its full-text index,
// once a program imports it. It is a package of its own so that a program
// that never searches text does not link the module.
//
//	import _ "github.com/tinyshed/tinystore/sqldb/fts5"
package fts5

import (
	"github.com/ncruces/go-sqlite3/ext/fts5"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

func init() {
	sqlite.RegisterModule([]string{"fts5"}, fts5.Register)
}
