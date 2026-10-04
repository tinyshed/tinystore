// Package rtree gives every sqldb database SQLite's R*Tree and Geopoly,
// indexes of ranges and shapes, once a program imports it. It is a package of
// its own so that a program that never makes one does not link the module.
//
//	import _ "github.com/tinyshed/tinystore/sqldb/rtree"
package rtree

import (
	"github.com/ncruces/go-sqlite3/ext/rtree"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

func init() {
	sqlite.RegisterModule([]string{"rtree", "rtree_i32", "geopoly"}, rtree.Register)
}
