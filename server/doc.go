// Package server serves a tinystore.Store to other processes through any byte
// stream, as docs/server.md designs it: a sidecar's clients in Bun and Python
// on the same machine and a remote server's alike. A handler turns a message
// into an engine's call and its answer into a message; claims, leases, group
// commits, snapshots and bounds stay the engines'. The bytes are server/wire.
//
//	server.go    New, Serve, ServeConn, Close: the server, its limits and its engines
//	session.go   a connection: its handshake, its reader, GOAWAY, silence
//	stream.go    a stream, an upload's inbox, and a call as its handler answers it
//	workers.go   the goroutines that run a connection's calls, and a call's one end
//	errors.go    an engine's error as a code and the item it names
//	listen.go    Listen: Unix sockets, TCP and TLS; pipe.go: Windows named pipes
//	tokens.go    the tokens a remote connection is let in with
//	handles.go   the numbers open calls answer with
//	methods.go   every method, by its number; kv.go: the kv engine's
package server
