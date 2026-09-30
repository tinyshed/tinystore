// Package client is the Go end of a connection, which the server's tests and
// measurements use: calls from many goroutines, each a stream of its own, and
// their answers in any order. A Go program embeds the store instead.
package client

import (
	"context"
	"crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"github.com/tinyshed/tinystore/server/internal/flow"
	"github.com/tinyshed/tinystore/server/internal/pipe"
	"github.com/tinyshed/tinystore/server/wire"
)

// Conn is a client's connection. Its reader never waits for the program: what
// the server sends waits on its stream, within the credit the client granted.
type Conn struct {
	conn    io.ReadWriteCloser
	writer  *flow.Writer
	reader  *wire.Reader
	Welcome wire.Welcome
	credit  *flow.Allowance // the connection's, client to server

	window int64 // the DATA the server may send on a stream before the client grants more

	mu      sync.Mutex
	streams map[uint32]*Stream
	next    uint32
	goAway  *wire.GoAway
	ended   error
	read    chan struct{} // closed when the reader stops
}

type Message interface {
	Append(dst []byte) []byte
}

// Dial connects to an endpoint, unix://, tcp:// or pipe:, and shakes hands.
func Dial(ctx context.Context, endpoint string, hello wire.Hello) (*Conn, error) {
	conn, err := dial(ctx, endpoint)
	if err != nil {
		return nil, err
	}
	return New(conn, hello)
}

// ErrNotTheServer is an endpoint whose WELCOME does not prove it read SERVE:
// the server that wrote SERVE is gone, and another process holds its endpoint.
var ErrNotTheServer = errors.New("client: the endpoint SERVE names cannot prove it read SERVE")

// Found reaches the sidecar that SERVE in <dir>/server/ names, as an SDK does.
// Its HELLO carries a fresh challenge, and no call goes before the WELCOME's
// proof checks.
func Found(ctx context.Context, dir string, hello wire.Hello) (*Conn, error) {
	text, err := os.ReadFile(filepath.Join(dir, "server", "SERVE")) //nolint:gosec // the store the caller names
	if err != nil {
		return nil, err
	}
	var published wire.Published
	if err = json.Unmarshal(text, &published); err != nil {
		return nil, fmt.Errorf("client: SERVE: %w", err)
	}
	if len(published.Endpoints) == 0 {
		return nil, errors.New("client: SERVE names no endpoint")
	}
	hello.Challenge = make([]byte, wire.ChallengeSize)
	if _, err = rand.Read(hello.Challenge); err != nil {
		return nil, err
	}
	conn, err := Dial(ctx, published.Endpoints[0], hello)
	if err != nil {
		return nil, err
	}
	if !published.Proves(hello.Challenge, conn.Welcome.Proof) {
		return nil, errors.Join(ErrNotTheServer, conn.Close())
	}
	return conn, nil
}

func dial(ctx context.Context, endpoint string) (io.ReadWriteCloser, error) {
	scheme, address, _ := strings.Cut(endpoint, ":")
	address = strings.TrimPrefix(address, "//")
	var dialer net.Dialer
	switch scheme {
	case "unix", "tcp":
		return dialer.DialContext(ctx, scheme, address)
	case "pipe":
		return pipe.Dial(ctx, address)
	}
	return nil, fmt.Errorf("client: no transport %q", scheme)
}

// New shakes hands over conn: HELLO, then the server's WELCOME, or its
// GOAWAY as an error.
func New(conn io.ReadWriteCloser, hello wire.Hello) (*Conn, error) {
	if hello.Protocol == 0 {
		hello.Protocol = wire.Protocol
	}
	if hello.Client == "" {
		hello.Client = "tinystore-go-test"
	}
	c := &Conn{
		conn: conn, writer: flow.NewWriter(conn, 4<<20), reader: wire.NewReader(conn, 64<<20),
		window: wire.DefaultStreamCredit, streams: map[uint32]*Stream{}, read: make(chan struct{}),
	}
	if hello.StreamCredit != 0 {
		c.window = int64(hello.StreamCredit)
	}
	sent := c.writer.Send(wire.AppendFrame(nil, wire.Header{Kind: wire.KindHello}, hello.Append(nil)))
	if err := c.welcome(); err != nil {
		return nil, errors.Join(err, sent, conn.Close())
	}
	c.credit = flow.NewAllowance(int64(c.Welcome.ConnectionCredit))
	c.reader.SetMaxBody(c.Welcome.MaxBody)
	go c.receive()
	return c, nil
}

func (c *Conn) welcome() error {
	h, err := c.reader.Header()
	if err != nil {
		return err
	}
	body, err := c.reader.Body(nil)
	if err != nil {
		return err
	}
	switch h.Kind {
	case wire.KindWelcome:
		return c.Welcome.Decode(body)
	case wire.KindGoAway:
		var goAway wire.GoAway
		if err := goAway.Decode(body); err != nil {
			return err
		}
		return &wire.Error{Code: goAway.Code, Message: goAway.Message}
	}
	return fmt.Errorf("client: a %s where the WELCOME belongs", h.Kind)
}

// Close ends the connection and waits for its reader.
func (c *Conn) Close() error {
	err := c.conn.Close()
	<-c.read
	return err
}

// CloseWrite ends what the client sends, as a private child's parent closes
// its stdin, and leaves the answers to come.
func (c *Conn) CloseWrite() error {
	if half, ok := c.conn.(interface{ CloseWrite() error }); ok {
		return half.CloseWrite()
	}
	return errors.New("client: this connection does not close one way")
}

// Done is closed once the connection ends.
func (c *Conn) Done() <-chan struct{} {
	return c.read
}

func (c *Conn) GoAway() *wire.GoAway {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.goAway
}

// SendFrame writes a frame as it is, for a test that breaks the protocol on
// purpose.
func (c *Conn) SendFrame(frame []byte) error {
	return c.writer.Send(frame)
}

// Adopt watches a stream a test opens with frames of its own.
func (c *Conn) Adopt(id uint32) (*Stream, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.streams[id] != nil {
		return nil, fmt.Errorf("client: stream %d is in use", id)
	}
	st := &Stream{conn: c, id: id, ready: make(chan struct{}, 1), window: flow.NewCredit(c.window)}
	c.streams[id] = st
	return st, nil
}

// Call sends a request and waits for its answer's body; an ERROR is a
// *wire.Error.
func (c *Conn) Call(ctx context.Context, method wire.Method, request Message) ([]byte, error) {
	st, err := c.Open(ctx, method, request, true)
	if err != nil {
		return nil, err
	}
	return st.Response(ctx)
}

// Open opens a stream with its REQUEST; end says the client sends nothing
// after it.
func (c *Conn) Open(ctx context.Context, method wire.Method, request Message, end bool) (*Stream, error) {
	body := request.Append(nil)
	if err := c.credit.Take(ctx, int64(len(body))); err != nil {
		return nil, err
	}
	st, err := c.newStream(end)
	if err != nil {
		return nil, err
	}
	flags := wire.Flags(0)
	if end {
		flags = wire.FlagEnd
	}
	header := wire.Header{Kind: wire.KindRequest, Flags: flags, Method: method, Stream: st.id}
	if err := c.writer.Send(wire.AppendFrame(nil, header, body)); err != nil {
		return nil, err
	}
	return st, nil
}

// newStream names a stream with a number not in use
func (c *Conn) newStream(end bool) (*Stream, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	switch {
	case c.goAway != nil:
		return nil, &wire.Error{Code: c.goAway.Code, Message: c.goAway.Message}
	case c.ended != nil:
		return nil, c.ended
	case len(c.streams) >= int(c.Welcome.InFlight):
		return nil, fmt.Errorf("client: %d streams in flight already", len(c.streams))
	}
	for {
		c.next++
		if c.next == 0 {
			c.next = 1
		}
		if c.streams[c.next] == nil {
			break
		}
	}
	st := &Stream{conn: c, id: c.next, ready: make(chan struct{}, 1)}
	st.window = flow.NewCredit(c.window)
	if !end {
		st.sends = flow.NewAllowance(int64(c.Welcome.StreamCredit))
		st.uploadDone = make(chan struct{})
	}
	c.streams[st.id] = st
	return st, nil
}

// receive takes frames until the connection ends, and hands each to its stream
func (c *Conn) receive() {
	defer close(c.read)
	err := c.take()
	c.mu.Lock()
	c.ended = fmt.Errorf("client: the connection ended: %w", err)
	streams := c.streams
	c.streams = map[uint32]*Stream{}
	c.mu.Unlock()
	for _, st := range streams {
		st.endUpload(c.ended)
		st.push(frame{err: c.ended})
	}
	c.credit.End(c.ended)
	c.writer.Stop(c.ended)
}

func (c *Conn) take() error {
	for {
		h, err := c.reader.Header()
		if err != nil {
			return err
		}
		body, err := c.reader.Body(nil)
		if err != nil {
			return err
		}
		if err = c.frame(h, body); err != nil {
			return err
		}
	}
}

func (c *Conn) frame(h wire.Header, body []byte) error {
	switch h.Kind {
	case wire.KindResponse, wire.KindData:
		c.deliver(h, body)
	case wire.KindCredit:
		c.grant(h, body)
	case wire.KindPing:
		return c.writer.Post(wire.AppendFrame(nil, wire.Header{Kind: wire.KindPong}, body))
	case wire.KindGoAway:
		var goAway wire.GoAway
		if err := goAway.Decode(body); err != nil {
			return err
		}
		c.mu.Lock()
		c.goAway = &goAway
		c.mu.Unlock()
	case wire.KindPong:
	default:
		return fmt.Errorf("client: a %s from the server", h.Kind)
	}
	return nil
}

// deliver hands a RESPONSE or a DATA to its stream, and takes the stream out
// of use at its final frame
func (c *Conn) deliver(h wire.Header, body []byte) {
	c.mu.Lock()
	st := c.streams[h.Stream]
	if st != nil && h.Flags&wire.FlagEnd != 0 {
		delete(c.streams, h.Stream)
	}
	c.mu.Unlock()
	if st == nil {
		return
	}
	if h.Flags&wire.FlagEnd != 0 {
		_, err := failed(frame{header: h, body: body})
		st.endUpload(err)
	}
	st.push(frame{header: h, body: body})
}

func (c *Conn) grant(h wire.Header, body []byte) {
	n := int64(wire.Granted(body))
	if h.Stream == 0 {
		c.credit.Grant(n)
		return
	}
	c.mu.Lock()
	st := c.streams[h.Stream]
	c.mu.Unlock()
	if st != nil && st.sends != nil {
		st.sends.Grant(n)
	}
}
