package server

import (
	"context"
	"errors"
	"fmt"
	"io"
	"sync"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/server/internal/flow"
	"github.com/tinyshed/tinystore/server/wire"
)

// stream is a client's REQUEST from its frame to the server's final frame on
// it, the one span in which its number is in use
type stream struct {
	id      uint32
	method  wire.Method
	session *session
	ctx     context.Context
	cancel  context.CancelCauseFunc

	upload *inbox          // the DATA the client sends; nil when its REQUEST ended its side
	sends  *flow.Allowance // what the server may still send, once a download began; under session.mu
	lost   error           // why no DATA or CREDIT will come; under session.mu
}

var errCancelled = errors.New("the client cancelled the stream")

func newStream(s *session, h wire.Header) *stream {
	st := &stream{id: h.Stream, method: h.Method, session: s}
	st.ctx, st.cancel = context.WithCancelCause(s.ctx)
	if h.Flags&wire.FlagEnd == 0 {
		st.upload = &inbox{credit: flow.NewCredit(int64(s.agreed.streamCredit)), ready: make(chan struct{}, 1)}
	}
	return st
}

// receive puts a DATA body in the stream's upload, refusing DATA a stream
// does not take and DATA past its credit
func (st *stream) receive(body []byte, end bool) error {
	if st.upload == nil {
		giveBody(body)
		return fmt.Errorf("%w: DATA on stream %d, whose REQUEST ended the client's side", wire.ErrProtocol, st.id)
	}
	return st.upload.push(st.id, body, end)
}

// lose tells the stream that no DATA or CREDIT will come
func (st *stream) lose(err error) {
	s := st.session
	s.mu.Lock()
	if st.lost == nil {
		st.lost = err
	}
	sends := st.sends
	s.mu.Unlock()
	if st.upload != nil {
		st.upload.fail(err)
	}
	if sends != nil {
		sends.End(err)
	}
}

// inbox holds the DATA a client sent on a stream until its handler takes it.
// The stream's credit bounds its bytes and every DATA but the last carries at
// least one, so a push never waits.
type inbox struct {
	credit *flow.Credit
	mu     sync.Mutex
	bodies [][]byte
	ended  bool  // DATA·END came
	taken  bool  // the handler took the last body
	err    error // the client left or cancelled
	ready  chan struct{}
}

func (in *inbox) push(stream uint32, body []byte, end bool) error {
	if !in.credit.Receive(int64(len(body))) {
		giveBody(body)
		return fmt.Errorf("%w: DATA past stream %d's credit", wire.ErrProtocol, stream)
	}
	in.mu.Lock()
	if in.ended {
		in.mu.Unlock()
		giveBody(body)
		return fmt.Errorf("%w: DATA after the END of stream %d", wire.ErrProtocol, stream)
	}
	in.bodies = append(in.bodies, body)
	in.ended = end
	in.mu.Unlock()
	in.wake()
	return nil
}

func (in *inbox) fail(err error) {
	in.mu.Lock()
	if in.err == nil && !in.taken {
		in.err = err
	}
	in.mu.Unlock()
	in.wake()
}

func (in *inbox) wake() {
	select {
	case in.ready <- struct{}{}:
	default:
	}
}

// next waits for the next body the client sent; last says the client sent
// nothing after it. After the last it is io.EOF.
func (in *inbox) next(ctx context.Context) (body []byte, last bool, err error) {
	for {
		in.mu.Lock()
		switch {
		case in.err != nil:
			err = in.err
			in.mu.Unlock()
			return nil, false, err
		case len(in.bodies) > 0:
			body = in.bodies[0]
			in.bodies[0] = nil
			in.bodies = in.bodies[1:]
			last = in.ended && len(in.bodies) == 0
			in.taken = last
			in.mu.Unlock()
			return body, last, nil
		case in.taken:
			in.mu.Unlock()
			return nil, false, io.EOF
		}
		in.mu.Unlock()
		select {
		case <-in.ready:
		case <-ctx.Done():
			return nil, false, context.Cause(ctx)
		}
	}
}

// drain takes what the handler left, for its credit to be given back
func (in *inbox) drain() [][]byte {
	in.mu.Lock()
	defer in.mu.Unlock()
	left := in.bodies
	in.bodies = nil
	return left
}

// call is a stream as its handler sees it: the request it decodes, and the
// frames that answer it, built in its worker's buffer
type call struct {
	*stream
	request []byte
	frame   []byte // the worker's, which the writer copies what it sends from
	began   bool   // a RESPONSE without END went, and the answer goes on as DATA
	ended   bool   // the final frame went
}

// message is anything that appends itself as a body
type message interface {
	Append(dst []byte) []byte
}

// respond ends a call with its answer.
func respond[M message](c *call, m M) error {
	frame := m.Append(c.header(wire.KindResponse, wire.FlagEnd))
	if err := c.fits(frame); err != nil {
		return err
	}
	return c.final(frame)
}

// begin answers a download, or a stream both ways, whose items follow as DATA.
func begin[M message](c *call, m M) error {
	frame := m.Append(c.header(wire.KindResponse, 0))
	if err := c.fits(frame); err != nil {
		return err
	}
	wire.SetLength(frame)
	c.began = true
	c.openSends()
	return c.sent(frame, c.session.send(frame))
}

// item sends one item of a download as DATA, within the client's credit.
func item[M message](c *call, m M) error {
	frame := m.Append(c.header(wire.KindData, 0))
	if err := c.fits(frame); err != nil {
		return err
	}
	return c.data(frame)
}

// chunk sends bytes of a download as DATA within the client's credit; last
// ends the download with them.
func (c *call) chunk(p []byte, last bool) error {
	if !last {
		return c.data(append(c.header(wire.KindData, 0), p...))
	}
	frame := append(c.header(wire.KindData, wire.FlagEnd), p...)
	if err := c.take(frame); err != nil {
		return err
	}
	return c.final(frame)
}

// trailer ends a download with its last DATA, which says where the next page
// begins.
func trailer[M message](c *call, m M) error {
	frame := m.Append(c.header(wire.KindData, wire.FlagEnd))
	if err := c.fits(frame); err != nil {
		return err
	}
	if err := c.take(frame); err != nil {
		return err
	}
	return c.final(frame)
}

// fits refuses a message past the body the client agreed to take, which it
// would read as a broken protocol: its stream fails with limit instead
func (c *call) fits(frame []byte) error {
	body, most := len(frame)-wire.HeaderSize, int(c.session.agreed.maxBody)
	if body <= most {
		return nil
	}
	c.frame = frame[:0]
	return fmt.Errorf("%w: an answer of %d bytes, past the %d a body holds", tinystore.ErrLimit, body, most)
}

func (c *call) header(kind wire.Kind, flags wire.Flags) []byte {
	return wire.AppendHeader(c.frame[:0], wire.Header{Kind: kind, Flags: flags, Stream: c.id})
}

// openSends gives the stream what the client lets the server send on it
func (c *call) openSends() {
	s := c.session
	sends := flow.NewAllowance(int64(s.agreed.downloadCredit))
	s.mu.Lock()
	c.sends = sends
	lost := c.lost
	s.mu.Unlock()
	if lost != nil {
		sends.End(lost)
	}
}

// data sends a DATA frame once the client's credit lets its body go
func (c *call) data(frame []byte) error {
	if err := c.take(frame); err != nil {
		return err
	}
	return c.sent(frame, c.session.send(frame))
}

func (c *call) take(frame []byte) error {
	wire.SetLength(frame)
	if !c.began {
		return errors.New("server: DATA before the RESPONSE that begins its download")
	}
	return c.sends.Take(c.ctx, int64(len(frame)-wire.HeaderSize))
}

// final sends the stream's last frame, once
func (c *call) final(frame []byte) error {
	wire.SetLength(frame)
	c.ended = true
	return c.sent(frame, c.session.finish(c.stream, frame))
}

// sent keeps the buffer a frame grew for the worker's next one
func (c *call) sent(frame []byte, err error) error {
	c.frame = frame[:0]
	return err
}

// fail ends the stream with its error: the RESPONSE's, or the last DATA's
// once a download began
func (c *call) fail(err error) error {
	kind := wire.KindResponse
	if c.began {
		kind = wire.KindData
	}
	failure := failure(c.ctx, err)
	return c.final(failure.Append(c.header(kind, wire.FlagEnd|wire.FlagError)))
}

// receive waits for the next body of an upload; last says the client sent
// nothing after it. The handler gives each body back with consumed once it
// has used it.
func (c *call) receive() (body []byte, last bool, err error) {
	if c.upload == nil {
		return nil, false, fmt.Errorf("%w: this method takes DATA; its REQUEST must not end the stream",
			wire.ErrMessage)
	}
	return c.upload.next(c.ctx)
}

// consumed gives back a body the handler has used, and the credit it held
func (c *call) consumed(body []byte) {
	n := int64(len(body))
	giveBody(body)
	if grant := c.upload.credit.Consume(n); grant > 0 && !c.ended {
		credit := wire.AppendCredit(nil, c.id, uint32(grant)) //nolint:gosec // within the stream's credit
		c.session.stopUnless(c.session.writer.Send(credit))
	}
	c.session.letGo(n)
}
