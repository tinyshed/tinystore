// Package reach is how a program that does not hold a store reaches the server
// a directory's SERVE names, as an SDK does: a fresh challenge in its HELLO,
// and no call before the WELCOME's proof checks. The tinystore command reads
// a store through it, for its logs and its MCP tools; a Go program that holds
// the store embeds it instead.
package reach

import (
	"context"

	"github.com/tinyshed/tinystore/server/internal/client"
	"github.com/tinyshed/tinystore/server/wire"
)

// Conn is a connection to a server: calls from many goroutines, each a stream
// of its own, and their answers in any order.
type Conn = client.Conn

// Stream is one call's stream, for a method that answers a download.
type Stream = client.Stream

// Message is a request a call sends: one of server/wire's messages.
type Message = client.Message

// ErrNotTheServer is an endpoint that could not prove it read SERVE: the
// server that wrote SERVE is gone, and another process holds its endpoint.
var ErrNotTheServer = client.ErrNotTheServer

// Found connects to the server SERVE in <dir>/server/ names, introducing the
// client as name, and returns once the server has proved it read SERVE.
func Found(ctx context.Context, dir, name string) (*Conn, error) {
	return client.Found(ctx, dir, wire.Hello{Client: name})
}
