package server

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/server/internal/client"
	"github.com/tinyshed/tinystore/server/internal/pipe"
	"github.com/tinyshed/tinystore/server/wire"
)

// each local transport carries calls, and a connection through it is admin
func TestEachLocalTransportServesCalls(t *testing.T) {
	ts := startTestServer(t, Options{})
	dir, err := os.MkdirTemp("", "ts")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })

	endpoints := []string{"unix://" + filepath.Join(dir, "s.sock")}
	if pipe.Supported {
		endpoints = append(endpoints, fmt.Sprintf("pipe:tinystore-test-%d-%d", os.Getpid(), time.Now().UnixNano()))
	}
	for _, endpoint := range endpoints {
		t.Run(endpoint, func(t *testing.T) {
			l, err := Listen(t.Context(), endpoint, nil)
			if err != nil {
				t.Fatal(err)
			}
			if l.Remote() || l.Addr() != endpoint {
				t.Fatalf("%s: remote %v", l.Addr(), l.Remote())
			}
			served := make(chan error, 1)
			go func() { served <- ts.server.Serve(context.Background(), l) }()

			for range 3 {
				conn, err := client.Dial(t.Context(), endpoint, wire.Hello{})
				if err != nil {
					t.Fatal(err)
				}
				if conn.Welcome.Capability != wire.Admin {
					t.Fatalf("a local connection is %s", conn.Welcome.Capability)
				}
				echo(t, conn, 11)
				_ = conn.Close()
			}

			if err := l.Close(); err != nil {
				t.Fatal(err)
			}
			if err := <-served; err != nil {
				t.Fatalf("serve after its listener closed: %v", err)
			}
		})
	}
}

// the second server on a pipe's name is refused, so that no process holds a
// name before the one that owns it
func TestAPipesNameHasOneOwner(t *testing.T) {
	if !pipe.Supported {
		t.Skip("named pipes are Windows'")
	}
	name := fmt.Sprintf("tinystore-test-%d-%d", os.Getpid(), time.Now().UnixNano())
	first, err := pipe.Listen(name)
	if err != nil {
		t.Fatal(err)
	}
	defer first.Close()
	if second, err := pipe.Listen(name); err == nil {
		_ = second.Close()
		t.Fatal("a second listener took the pipe's name")
	}
}
