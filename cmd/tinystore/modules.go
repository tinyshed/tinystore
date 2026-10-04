package main

// Bun and Python reach their SQL databases through this binary and cannot
// link a module of their own, so it links SQLite's full-text and range indexes.
import (
	_ "github.com/tinyshed/tinystore/sqldb/fts5"
	_ "github.com/tinyshed/tinystore/sqldb/rtree"
)
