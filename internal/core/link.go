package core

import (
	"context"
	"errors"
	"io"
	"sync"
	"time"

	"github.com/tinyshed/tinystore/internal/wire"
)

// HandshakeTime is how long a connection waits for its WELCOME.
const HandshakeTime = 5 * time.Second

// Transport is a byte stream to a server: Write may block until the bytes
// left, Close ends it.
type Transport interface {
	Write([]byte) error
	Close() error
}

// Connection is a transport and the session over it.
type Connection struct {
	Session   *Session
	transport Transport
	// closing waits for what the connection ran to let go: a private child, the core in this process
	closing func()
	mu      sync.Mutex
	handles map[handleKey]*opening
}

type handleKey struct {
	method uint16
	body   string
	// opener is the opener of a handle kept in a database's file, whose bytes the connection makes
	opener *Opener
}

type opening struct {
	done   chan struct{}
	handle uint64
	err    error
}

// NewConnection is a connection of a session over a transport.
func NewConnection(session *Session, transport Transport, closing func()) *Connection {
	return &Connection{Session: session, transport: transport, closing: closing, handles: map[handleKey]*opening{}}
}

// Handshake waits for the WELCOME, and closes the transport once the session ends.
func (c *Connection) Handshake(ctx context.Context) error {
	ctx, cancel := context.WithTimeout(ctx, HandshakeTime)
	defer cancel()
	if _, err := c.Session.Welcomed(ctx); err != nil {
		_ = c.transport.Close()
		if errors.Is(err, context.DeadlineExceeded) {
			err = Errorf(Closed, "no WELCOME within %s", HandshakeTime)
		}
		c.Session.End(err)
		return err
	}
	c.Session.OnEnd(func(error) { _ = c.transport.Close() })
	return nil
}

// Close ends the connection; a private child or the core in this process is
// waited for until it let go.
func (c *Connection) Close() {
	c.Session.End(Errorf(Closed, "the store closed"))
	_ = c.transport.Close()
	if c.closing != nil {
		c.closing()
	}
}

// Read reads a transport into its session until it ends, then ends the session.
func Read(r io.Reader, session *Session, ended func(error) error) {
	buffer := make([]byte, 64<<10)
	for {
		n, err := r.Read(buffer)
		if n > 0 {
			session.Receive(buffer[:n])
		}
		if err != nil {
			if errors.Is(err, io.EOF) {
				err = Errorf(Closed, "the server ended the connection")
			}
			session.End(ended(err))
			return
		}
	}
}

// Idempotence is what a call may do when its connection is lost: go again,
// or say its outcome is unknown.
type Idempotence int

const (
	// Reading goes again when its connection is lost, its REQUEST sent or not.
	Reading Idempotence = iota
	// Writing goes again only when its REQUEST had not left; one that had is OutcomeUnknown.
	Writing
)

// Link gives calls a connection, dialling again once the last one ended. A
// call whose REQUEST had not left goes again when its connection is lost, so
// does a read that had, and a write that had is OutcomeUnknown.
type Link struct {
	dial    func(ctx context.Context) (*Connection, error)
	mu      sync.Mutex
	current *dialling
	closed  bool
}

type dialling struct {
	done       chan struct{}
	connection *Connection
	err        error
}

// NewLink is a link that dials with dial.
func NewLink(dial func(ctx context.Context) (*Connection, error)) *Link {
	return &Link{dial: dial}
}

// Closed says whether the link was closed.
func (l *Link) Closed() bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.closed
}

// Connection is the connection a call goes on: the one open, or a new one,
// one dial at a time, so that every caller that found the same dead
// connection waits for the one new one.
func (l *Link) Connection(ctx context.Context) (*Connection, error) {
	for {
		l.mu.Lock()
		if l.closed {
			l.mu.Unlock()
			return nil, Errorf(Closed, "the store is closed")
		}
		current := l.current
		if current == nil {
			current = &dialling{done: make(chan struct{})}
			l.current = current
			l.mu.Unlock()
			go func() {
				// the dial outlives a caller that gives up, so that the next finds it
				current.connection, current.err = l.dial(context.WithoutCancel(ctx))
				close(current.done)
			}()
		} else {
			l.mu.Unlock()
		}
		select {
		case <-current.done:
		case <-ctx.Done():
			return nil, ctx.Err()
		}
		if current.err == nil && current.connection.Session.Ended() == nil && !current.connection.Session.GoingAway() {
			return current.connection, nil
		}
		l.mu.Lock()
		if l.current == current {
			l.current = nil
		}
		l.mu.Unlock()
		if current.err != nil {
			return nil, current.err
		}
	}
}

// Run runs a call on the link's connection, going again as its idempotence lets it.
func (l *Link) Run(ctx context.Context, idempotence Idempotence, attempt func(c *Connection) error) error {
	for tries := 0; ; tries++ {
		c, err := l.Connection(ctx)
		if err != nil {
			return err
		}
		err = attempt(c)
		if err == nil {
			return nil
		}
		again, err := goesAgain(err, idempotence)
		if !again || tries >= 2 {
			return err
		}
	}
}

// Call is one call of a method on the link's connection.
func (l *Link) Call(ctx context.Context, idempotence Idempotence, method uint16, body []byte) ([]byte, error) {
	var answer []byte
	err := l.Run(ctx, idempotence, func(c *Connection) (err error) {
		answer, err = c.Session.Call(ctx, method, body)
		return err
	})
	return answer, err
}

// goesAgain says whether a call that failed goes on another connection: one
// whose REQUEST had not left, and a read that had. A write that had is
// OutcomeUnknown; any other failure is the call's own.
func goesAgain(err error, idempotence Idempotence) (bool, error) {
	if lost, sent := IsConnectionLost(err); lost {
		if !sent || idempotence == Reading {
			return true, err
		}
		return false, Wrap(OutcomeUnknown, err,
			"the connection was lost with the write in flight; read what it wrote before writing again")
	}
	if KindOf(err) == Unavailable {
		return true, err
	}
	return false, err
}

// Close closes the connection and every one to come.
func (l *Link) Close() {
	l.mu.Lock()
	l.closed = true
	current := l.current
	l.current = nil
	l.mu.Unlock()
	if current == nil {
		return
	}
	<-current.done
	if current.connection != nil {
		current.connection.Close()
	}
}

// Opener is what opens a handle: its open's bytes, or, for a handle kept in a
// database's file, what makes them once the database is open on the
// connection, since the open names the database by its handle there.
type Opener struct {
	Method uint16
	Body   []byte
	Make   func(ctx context.Context, c *Connection) ([]byte, error)
}

// HandleOn is the handle an open answers on a connection, opened once a
// connection. The open's own bytes name it, so a bucket opened twice with the
// same options shares its handle. An open that fails is tried again by the next call.
func HandleOn(ctx context.Context, c *Connection, opener *Opener) (uint64, error) {
	key := handleKey{method: opener.Method, body: string(opener.Body)}
	if opener.Make != nil {
		key = handleKey{method: opener.Method, opener: opener}
	}
	c.mu.Lock()
	o, found := c.handles[key]
	if !found {
		o = &opening{done: make(chan struct{})}
		c.handles[key] = o
		go func() {
			o.handle, o.err = c.open(context.WithoutCancel(ctx), opener)
			if o.err != nil {
				c.mu.Lock()
				if c.handles[key] == o {
					delete(c.handles, key)
				}
				c.mu.Unlock()
			}
			close(o.done)
		}()
	}
	c.mu.Unlock()
	select {
	case <-o.done:
		return o.handle, o.err
	case <-ctx.Done():
		return 0, ctx.Err()
	}
}

func (c *Connection) open(ctx context.Context, opener *Opener) (uint64, error) {
	body := opener.Body
	if opener.Make != nil {
		var err error
		if body, err = opener.Make(ctx, c); err != nil {
			return 0, err
		}
		inner := &Opener{Method: opener.Method, Body: body}
		return HandleOn(ctx, c, inner)
	}
	answer, err := c.Session.Call(ctx, opener.Method, body)
	if err != nil {
		return 0, err
	}
	var handle wire.Handle
	if err := wire.Decode(answer, &handle); err != nil {
		return 0, Wrap(Protocol, err, "a handle that does not read")
	}
	return handle.Handle, nil
}

// Via is how a handle's calls reach the store: on whichever connection the
// link has, or inside a database's transaction, on its stream.
type Via interface {
	Call(ctx context.Context, opener *Opener, method uint16, body func(handle uint64) []byte, idempotence Idempotence) ([]byte, error)
}

// ViaLink calls on whichever connection the link has, dialled again as it says.
type ViaLink struct{ Link *Link }

func (v ViaLink) Call(
	ctx context.Context, opener *Opener, method uint16, body func(handle uint64) []byte, idempotence Idempotence,
) ([]byte, error) {
	var answer []byte
	err := v.Link.Run(ctx, idempotence, func(c *Connection) error {
		handle, err := HandleOn(ctx, c, opener)
		if err != nil {
			return err
		}
		answer, err = c.Session.Call(ctx, method, body(handle))
		return err
	})
	return answer, err
}

// Download runs a download to its end, giving each item's credit back as it
// arrives: its header, its items and the trailer that ended it. A download
// the server answers with a RESPONSE that ends it has no items and that
// RESPONSE for trailer.
func Download(ctx context.Context, c *Connection, method uint16, body []byte) (header []byte, items [][]byte, trailer []byte, err error) {
	st, err := c.Session.Open(ctx, method, body, true)
	if err != nil {
		return nil, nil, nil, err
	}
	head, err := st.Next(ctx)
	if err != nil {
		return nil, nil, nil, err
	}
	if head.End {
		return head.Body, nil, head.Body, nil
	}
	for {
		event, err := st.Next(ctx)
		if err != nil {
			return nil, nil, nil, err
		}
		st.Consumed(len(event.Body))
		if event.End {
			return head.Body, items, event.Body, nil
		}
		items = append(items, event.Body)
	}
}

// Piece is the bytes an upload's piece carries: half what the server takes on
// a stream before it grants more, less its own few.
func Piece(c *Connection) int {
	agreed := c.Session.Agreed()
	most := uint64(1 << 20)
	if agreed != nil {
		most = min(agreed.MaxBody, agreed.StreamCredit)
	}
	return max(1, int(most/2)-16)
}
