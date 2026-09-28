package server

import (
	"io"

	"github.com/tinyshed/tinystore/server/internal/pipe"
)

// listenPipe serves a Windows named pipe, local as a Unix socket is
func listenPipe(name string) (Listener, error) {
	l, err := pipe.Listen(name)
	if err != nil {
		return nil, err
	}
	return pipeListener{listener: l, name: name}, nil
}

type pipeListener struct {
	listener *pipe.Listener
	name     string
}

func (l pipeListener) Accept() (io.ReadWriteCloser, error) { return l.listener.Accept() }
func (l pipeListener) Close() error                        { return l.listener.Close() }
func (l pipeListener) Addr() string                        { return "pipe:" + l.name }
func (l pipeListener) Remote() bool                        { return false }
