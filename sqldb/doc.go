// Package sqldb gives an application its own SQLite databases inside a
// tinystore.Store. The application writes the SQL; a struct says what a row
// is, Table what a struct cannot say, and the engine owns the file, the
// connections, the migrations, the values and the transactions.
//
// What a call's name starts with says where it runs: Exec, ExecOne, ExecAll,
// ExecScalar, ExecQuery and Insert may write and go to the file's one writer;
// One, All, Each, Scalar and Query read, on readers that refuse to write.
//
// The package sqldbtest checks, in a program's tests, that a database's
// migrations make what its schema declares.
package sqldb
