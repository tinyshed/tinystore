//go:build !windows

// Package pipe serves and reaches a Windows named pipe; elsewhere a Unix
// socket is the local transport, and it has nothing to serve.
package pipe

import (
	"context"
	"errors"
	"io"
)

var errWindows = errors.New("named pipes are Windows'")

type Listener struct{}

func Listen(string) (*Listener, error) { return nil, errWindows }

func (*Listener) Accept() (io.ReadWriteCloser, error) { return nil, errWindows }
func (*Listener) Close() error                        { return nil }

func Dial(context.Context, string) (io.ReadWriteCloser, error) { return nil, errWindows }

func Path(name string) string { return name }

const Supported = false
