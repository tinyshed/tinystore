package core

import (
	"context"
	"crypto/rand"
	"crypto/tls"
	"io"
	"net"
	"regexp"
	"runtime"
	"runtime/debug"
	"strings"
	"sync"
)

// Version is this SDK's release, as the module a program depends on names
// it; 0.0.0 for a copy built from a checkout.
var Version = func() string {
	info, ok := debug.ReadBuildInfo()
	if !ok {
		return "0.0.0"
	}
	for _, dep := range info.Deps {
		if dep.Path == "github.com/tinyshed/tinystore" && strings.HasPrefix(dep.Version, "v") {
			return strings.TrimPrefix(dep.Version, "v")
		}
	}
	return "0.0.0"
}()

// Client is what a HELLO says this SDK is, for the server's logs.
var Client = "tinystore-go/" + Version + " " + strings.Replace(runtime.Version(), "go", "go/", 1)

var remoteEndpoint = regexp.MustCompile(`^(tcp|tls)://(\[[^\]]+\]|[^:/]+):(\d+)$`)

// Dial connects to unix://path, pipe:name, tcp://host:port or tls://host:port,
// shakes hands, and gives up after the handshake's time. A secret, SERVE's,
// asks the server to prove it read SERVE; a token admits a remote client.
func Dial(ctx context.Context, endpoint, token string, secret []byte, tlsConfig *tls.Config) (*Connection, error) {
	conn, err := connect(ctx, endpoint, tlsConfig)
	if err != nil {
		return nil, Wrap(Closed, err, "cannot reach %s", endpoint)
	}
	hello := Hello{Client: Client, Token: token, Secret: secret}
	if secret != nil {
		hello.Challenge = make([]byte, 16)
		_, _ = rand.Read(hello.Challenge)
	}
	var writing sync.Mutex
	session := NewSession(func(b []byte) error {
		writing.Lock()
		defer writing.Unlock()
		_, err := conn.Write(b)
		return err
	}, hello)
	go Read(conn, session, func(err error) error { return err })
	c := NewConnection(session, closer{conn}, nil)
	if err := c.Handshake(ctx); err != nil {
		return nil, err
	}
	return c, nil
}

type closer struct{ conn io.ReadWriteCloser }

func (c closer) Write(b []byte) error {
	_, err := c.conn.Write(b)
	return err
}

func (c closer) Close() error { return c.conn.Close() }

func connect(ctx context.Context, endpoint string, tlsConfig *tls.Config) (io.ReadWriteCloser, error) {
	switch {
	case strings.HasPrefix(endpoint, "unix://"):
		var dialer net.Dialer
		return dialer.DialContext(ctx, "unix", strings.TrimPrefix(endpoint, "unix://"))
	case strings.HasPrefix(endpoint, "pipe:"):
		return dialPipe(ctx, strings.TrimPrefix(endpoint, "pipe:"))
	}
	remote := remoteEndpoint.FindStringSubmatch(endpoint)
	if remote == nil {
		return nil, Errorf(Invalid, "no transport for the endpoint %s", endpoint)
	}
	host := strings.Trim(remote[2], "[]")
	address := net.JoinHostPort(host, remote[3])
	if remote[1] == "tls" {
		config := &tls.Config{MinVersion: tls.VersionTLS12}
		if tlsConfig != nil {
			config = tlsConfig.Clone()
		}
		if config.ServerName == "" {
			config.ServerName = host
		}
		dialer := tls.Dialer{Config: config}
		return dialer.DialContext(ctx, "tcp", address)
	}
	var dialer net.Dialer
	return dialer.DialContext(ctx, "tcp", address)
}
