package server

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"io"
	"net"
	"strings"
)

// Listener hands the server connections. Remote says they come from a
// network, and their HELLO must carry a token; a local one's permission is
// the file system's.
type Listener interface {
	Accept() (io.ReadWriteCloser, error)
	Close() error
	Addr() string // the endpoint, as SERVE names it
	Remote() bool
}

// Listen listens at an endpoint:
//
//	unix:///srv/app/data/server/tinystore.sock   a Unix socket, local
//	pipe:tinystore-1f3a…                         a Windows named pipe, local
//	tcp://0.0.0.0:7443                           TCP, remote
//	tls://0.0.0.0:7443                           TCP with TLS, remote; config is its certificate
func Listen(ctx context.Context, endpoint string, config *tls.Config) (Listener, error) {
	scheme, address, found := strings.Cut(endpoint, ":")
	if !found {
		return nil, fmt.Errorf("server: an endpoint is scheme:address, not %q", endpoint)
	}
	address = strings.TrimPrefix(address, "//")
	switch scheme {
	case "unix":
		return listenNet(ctx, "unix", address, endpoint, false)
	case "tcp":
		return listenNet(ctx, "tcp", address, "", true)
	case "tls":
		if config == nil {
			return nil, errors.New("server: tls needs a certificate")
		}
		var listening net.ListenConfig
		l, err := listening.Listen(ctx, "tcp", address)
		if err != nil {
			return nil, err
		}
		return netListener{listener: tls.NewListener(l, config), endpoint: "tls://" + l.Addr().String(), remote: true},
			nil
	case "pipe":
		return listenPipe(address)
	}
	return nil, fmt.Errorf("server: no transport %q in %q", scheme, endpoint)
}

func listenNet(ctx context.Context, network, address, endpoint string, remote bool) (Listener, error) {
	var listening net.ListenConfig
	l, err := listening.Listen(ctx, network, address)
	if err != nil {
		return nil, err
	}
	if endpoint == "" {
		endpoint = network + "://" + l.Addr().String()
	}
	return netListener{listener: l, endpoint: endpoint, remote: remote}, nil
}

type netListener struct {
	listener net.Listener
	endpoint string
	remote   bool
}

func (l netListener) Accept() (io.ReadWriteCloser, error) { return l.listener.Accept() }
func (l netListener) Close() error                        { return l.listener.Close() }
func (l netListener) Addr() string                        { return l.endpoint }
func (l netListener) Remote() bool                        { return l.remote }
