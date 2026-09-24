// Package sqldb gives an application its own SQLite databases inside a
// tinystore.Store. The application writes the schema and the SQL; the engine
// owns the file, the connections, the migrations and the transactions.
//
// What a call's name starts with says where it runs: Exec, ExecOne, ExecAll
// and ExecScalar may write and go to the file's one writer; One, All and Scalar
// read, on readers that refuse to write.
//
//	db.go      Open, Exec, Tx, Close: one database in sql/<name>.db
//	read.go    One, All, Scalar and their Exec forms
//	scan.go    a row into a struct, by db tag or snake_case field name
package sqldb
