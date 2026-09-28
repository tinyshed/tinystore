// Package sqldb gives an application its own SQLite databases inside a
// tinystore.Store. The application writes the SQL; a struct says what a row
// is, Table what a struct cannot say, and the engine owns the file, the
// connections, the migrations, the values and the transactions.
//
// What a call's name starts with says where it runs: Exec, ExecOne, ExecAll,
// ExecScalar and Insert may write and go to the file's one writer; One, All,
// Each and Scalar read, on readers that refuse to write.
//
//	db.go      Open, Close, Snapshot: one database in sql/<name>.db
//	check.go   Open's check of the file against the schema
//	table.go   Table and its options: what a declaration says of one table
//	schema.go  Schema, and the SQL it prints
//	ddl.go     a table's CREATE TABLE, its indexes, Insert's statement, a default's literal
//	model.go   a struct's columns, by db tag or snake_case field name
//	values.go  a Go type as its column stores it, and a value read back into it
//	types.go   JSON and Date
//	plan.go    the field each column of a query fills, found once a type and its columns
//	read.go    One, All, Each, Scalar and their Exec forms
//	write.go   Exec and Insert
//	tx.go      Tx and View
//	call.go    where a call runs, and the store's memory it holds
//	errors.go  ConstraintError, and what an error from SQLite means
//
// The package sqldbtest checks, in a program's tests, that a database's
// migrations make what its schema declares.
package sqldb
