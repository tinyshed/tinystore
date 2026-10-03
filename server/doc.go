// Package server serves a tinystore.Store to other processes through any byte
// stream, as research's design/server.md designed it: a sidecar's clients in
// Bun and Python on the same machine and a remote server's alike. A handler
// turns a message into an engine's call and its answer into a message; claims,
// leases, group commits, snapshots and bounds stay the engines'. The bytes are
// server/wire, laid out in docs/wire.md.
//
//	server.go    New, Serve, ServeConn, Close: the server, its limits and its engines
//	session.go   a connection: its handshake, its reader, GOAWAY, silence
//	stream.go    a stream, an upload's inbox, and a call as its handler answers it
//	workers.go   the goroutines that run a connection's calls, and a call's one end
//	errors.go    an engine's error as a code and the item it names
//	listen.go    Listen: Unix sockets, TCP and TLS; pipe.go: Windows named pipes
//	local.go     Publish: the directory's shared sidecar and its SERVE; Share: a program's own store
//	tokens.go    the tokens a remote connection is let in with
//	handles.go   the numbers open calls answer with
//	bodies.go    the pools frame bodies are read into
//	datasql.go   the check a data connection's SQL passes; sqltokens.go: SQLite's tokens
//	methods.go   every method, by its number; clock.go: a test's clock, which server.clock moves
//	kv.go, jobs.go, jobs_work.go, blobs.go, records.go, metrics.go, sql.go: each engine's handlers
package server
