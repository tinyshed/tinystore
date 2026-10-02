package server

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"os"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore/server/internal/flow"
	"github.com/tinyshed/tinystore/server/wire"
)

// session is one connection: its handshake, the frames its reader takes, and
// the streams in flight with the handles they carry. Its reader never waits
// for the client: what it sends goes out through Post, and every frame it
// takes fits room the connection's credit already granted.
type session struct {
	server *Server
	conn   io.ReadWriteCloser
	remote bool
	log    *slog.Logger
	reader *wire.Reader
	writer *flow.Writer

	capability wire.Capability
	agreed     agreed
	credit     *flow.Credit // the connection's, client to server

	parent context.Context // what ServeConn was given, whose end ends the connection
	ctx    context.Context
	cancel context.CancelCauseFunc

	mu        sync.Mutex
	streams   map[uint32]*stream
	goingAway bool
	gone      bool // the reader stopped, so nothing waits for the client

	workers  workers
	heard    atomic.Uint64 // frames read, which the silence watch compares
	closing  sync.Once
	closeErr error // what closing the connection said, once it has

	kvHandles    handles[*kvHandle]
	jobsHandles  handles[*jobsHandle]
	claims       handles[heldJob] // jobs a jobs.claim leased, until their settlement
	blobsHandles handles[*blobsHandle]
	sqlHandles   handles[*sqlHandle]
}

type agreed struct {
	maxBody        uint32 // the largest body either side sends
	inFlight       uint32 // the streams a client may have open at once
	streamCredit   uint32 // DATA a client sends on a stream before credit comes back
	downloadCredit uint32 // DATA the server sends on a stream before the client grants more
}

func newSession(ctx context.Context, server *Server, conn io.ReadWriteCloser, remote bool) *session {
	s := &session{
		server: server, conn: conn, remote: remote, log: server.log,
		reader:  wire.NewReader(conn, server.limits.maxBody),
		writer:  flow.NewWriter(conn, server.limits.queuedAnswers),
		credit:  flow.NewCredit(int64(server.limits.connectionCredit)),
		streams: map[uint32]*stream{},
	}
	s.parent = ctx
	s.ctx, s.cancel = context.WithCancelCause(ctx)
	s.workers.calls = make(chan *call, server.limits.inFlight)
	return s
}

// serve runs the connection until it ends: a clean end of what the client
// sends drains the streams running and is nil
func (s *session) serve() error {
	stop := context.AfterFunc(s.parent, func() { s.stop(context.Cause(s.parent)) })
	defer stop()
	if err := s.handshake(); err != nil {
		s.close()
		return errors.Join(err, s.closeErr)
	}
	watch := s.watchSilence()
	defer watch.Stop()
	err := s.end(s.read())
	s.close()
	return errors.Join(err, s.closeErr)
}

var (
	errHandshake       = errors.New("no HELLO within the handshake's time")
	errSilent          = errors.New("the client said nothing past a PING")
	errUnauthenticated = errors.New("a remote connection's HELLO carries no token the server knows")
)

// handshake takes the client's HELLO and answers WELCOME, or GOAWAY for a
// HELLO it cannot take
func (s *session) handshake() error {
	timeout := time.AfterFunc(s.server.limits.handshake, func() { s.stop(errHandshake) })
	defer timeout.Stop()

	h, err := s.reader.Header()
	if err != nil {
		return s.refuse(wire.CodeProtocol, err)
	}
	body, err := s.reader.Body(nil)
	switch {
	case err != nil:
		return s.refuse(wire.CodeProtocol, err)
	case h.Kind != wire.KindHello:
		return s.refuse(wire.CodeProtocol, fmt.Errorf("%w: the first frame is a HELLO, not a %s", wire.ErrProtocol,
			h.Kind))
	}
	var hello wire.Hello
	if err := wire.SkipUnknown(hello.Decode(body)); err != nil {
		return s.refuse(wire.CodeProtocol, err)
	}
	if hello.Protocol < wire.OldestProtocol {
		return s.refuse(wire.CodeProtocol, fmt.Errorf("%w: this server speaks protocols %d to %d, not %d",
			wire.ErrProtocol, wire.OldestProtocol, wire.Protocol, hello.Protocol))
	}
	if hello.Challenge != nil && len(hello.Challenge) != wire.ChallengeSize {
		return s.refuse(wire.CodeProtocol, fmt.Errorf("%w: a challenge of %d bytes, not %d", wire.ErrProtocol,
			len(hello.Challenge), wire.ChallengeSize))
	}
	if !s.admit(hello) {
		return s.refuse(wire.CodeUnauthenticated, errUnauthenticated)
	}

	s.agree(hello)
	s.server.log.Info("connected", "client", hello.Client)
	return s.writer.Send(wire.AppendFrame(nil, wire.Header{Kind: wire.KindWelcome}, s.welcome(hello).Append(nil)))
}

// admit gives a local connection admin, and a remote one what its token says
func (s *session) admit(hello wire.Hello) bool {
	if !s.remote {
		s.capability = wire.Admin
		return true
	}
	capability, known := s.server.options.Tokens.capability(hello.Token)
	s.capability = capability
	return known
}

// agree settles the bounds: a body no larger than either side takes, nor than
// the credit a stream starts with, so that every body can be sent
func (s *session) agree(hello wire.Hello) {
	bounds := s.server.limits
	download := uint32(wire.DefaultStreamCredit)
	if hello.StreamCredit != 0 {
		download = hello.StreamCredit
	}
	most := min(bounds.maxBody, download)
	if hello.MaxBody != 0 {
		most = min(most, hello.MaxBody)
	}
	s.agreed = agreed{
		maxBody: most, inFlight: bounds.inFlight, streamCredit: bounds.streamCredit,
		downloadCredit: download,
	}
	s.reader.SetMaxBody(most)
}

// welcome answers a HELLO; a local one's challenge gets its proof, which a
// remote client, whose server TLS proves, has no SERVE to check
func (s *session) welcome(hello wire.Hello) wire.Welcome {
	welcome := wire.Welcome{
		Protocol: min(hello.Protocol, wire.Protocol), Server: s.server.options.Version, Instance: s.server.instance,
		Capability: s.capability, MaxBody: s.agreed.maxBody, InFlight: s.agreed.inFlight,
		ConnectionCredit: s.server.limits.connectionCredit, StreamCredit: s.agreed.streamCredit,
		Engines: s.server.engines(), Now: s.server.store.Now().UnixMilli(),
	}
	if hello.Challenge != nil && !s.remote {
		welcome.Proof = wire.Prove(s.server.secret, hello.Challenge)
	}
	return welcome
}

// refuse ends a connection whose HELLO it cannot take with a GOAWAY
func (s *session) refuse(code wire.Code, err error) error {
	if errors.Is(err, io.EOF) || s.ctx.Err() != nil {
		return err
	}
	return errors.Join(err, s.sayLast(wire.GoAway{Code: code, Message: err.Error()}))
}

// read takes frames until the connection ends or a frame breaks the protocol
func (s *session) read() error {
	for {
		h, err := s.reader.Header()
		if err != nil {
			return err
		}
		s.heard.Add(1)
		if err = s.take(h); err != nil {
			return err
		}
	}
}

func (s *session) take(h wire.Header) error {
	switch h.Kind {
	case wire.KindRequest:
		return s.request(h)
	case wire.KindData:
		return s.data(h)
	case wire.KindCancel:
		if _, err := s.reader.Body(nil); err != nil {
			return err
		}
		s.cancelStream(h.Stream)
		return nil
	case wire.KindCredit:
		return s.grant(h)
	case wire.KindPing:
		return s.pong()
	case wire.KindPong:
		_, err := s.reader.Body(nil)
		return err
	}
	return fmt.Errorf("%w: a %s from a client", wire.ErrProtocol, h.Kind)
}

// request opens the stream a REQUEST names and hands it to a worker; once the
// server is going away it answers unavailable without running it
func (s *session) request(h wire.Header) error {
	if !s.credit.Receive(int64(h.Length)) {
		return fmt.Errorf("%w: a REQUEST past the connection's credit", wire.ErrProtocol)
	}
	body, err := s.reader.Body(takeBody(h.Length))
	if err != nil {
		return err
	}
	st, err := s.open(h)
	switch {
	case err != nil:
		giveBody(body)
		return err
	case st == nil:
		giveBody(body)
		return errors.Join(s.letGoFromReader(int64(h.Length)), s.unavailable(h.Stream))
	}
	s.workers.run(s, &call{stream: st, request: body})
	return nil
}

// unavailable answers a REQUEST that crossed the GOAWAY without running it,
// so that the client may send it again elsewhere
func (s *session) unavailable(stream uint32) error {
	unavailable := &wire.Error{Code: wire.CodeUnavailable, Message: errClosing.Error()}
	header := wire.Header{Kind: wire.KindResponse, Flags: wire.FlagEnd | wire.FlagError, Stream: stream}
	return s.post(wire.AppendFrame(nil, header, unavailable.Append(nil)))
}

// open puts a stream in use: nil once the server is going away, and a
// protocol error for a number in use or one past the streams in flight
func (s *session) open(h wire.Header) (*stream, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	switch {
	case s.streams[h.Stream] != nil:
		return nil, fmt.Errorf("%w: a REQUEST on stream %d, which is in use", wire.ErrProtocol, h.Stream)
	case len(s.streams) >= int(s.agreed.inFlight):
		return nil, fmt.Errorf("%w: a REQUEST past the %d streams in flight", wire.ErrProtocol, s.agreed.inFlight)
	case s.goingAway:
		return nil, nil
	}
	st := newStream(s, h)
	s.streams[h.Stream] = st
	return st, nil
}

// data hands a DATA frame to the upload its stream is. DATA on a stream that
// ended, sent before the client learnt it, is dropped and its credit given
// back.
func (s *session) data(h wire.Header) error {
	if h.Length == 0 && h.Flags&wire.FlagEnd == 0 {
		return fmt.Errorf("%w: an empty DATA that does not end stream %d", wire.ErrProtocol, h.Stream)
	}
	if h.Flags&wire.FlagError != 0 {
		return fmt.Errorf("%w: DATA with ERROR from a client on stream %d; it cancels", wire.ErrProtocol, h.Stream)
	}
	if !s.credit.Receive(int64(h.Length)) {
		return fmt.Errorf("%w: DATA past the connection's credit", wire.ErrProtocol)
	}
	body, err := s.reader.Body(takeBody(h.Length))
	if err != nil {
		return err
	}
	s.mu.Lock()
	st := s.streams[h.Stream]
	s.mu.Unlock()
	if st == nil {
		giveBody(body)
		return s.letGoFromReader(int64(h.Length))
	}
	return st.receive(body, h.Flags&wire.FlagEnd != 0)
}

func (s *session) cancelStream(id uint32) {
	s.mu.Lock()
	st := s.streams[id]
	s.mu.Unlock()
	if st != nil {
		st.cancel(errCancelled)
		st.lose(errCancelled)
	}
}

// grant hands a client's CREDIT to the download on its stream
func (s *session) grant(h wire.Header) error {
	body, err := s.reader.Body(nil)
	if err != nil {
		return err
	}
	if h.Stream == 0 {
		return fmt.Errorf("%w: a client's CREDIT for the connection, which only the server grants", wire.ErrProtocol)
	}
	s.mu.Lock()
	st := s.streams[h.Stream]
	var sends *flow.Allowance
	if st != nil {
		sends = st.sends
	}
	s.mu.Unlock()
	if sends != nil {
		sends.Grant(int64(wire.Granted(body)))
	}
	return nil
}

func (s *session) pong() error {
	body, err := s.reader.Body(nil)
	if err != nil {
		return err
	}
	return s.post(wire.AppendFrame(nil, wire.Header{Kind: wire.KindPong}, body))
}

// post sends a frame from the reader, which never waits
func (s *session) post(frame []byte) error {
	return s.writer.Post(frame)
}

// send writes a frame from a worker; a write that fails ends the connection,
// and every stream learns it through its context
func (s *session) send(frame []byte) error {
	err := s.writer.Send(frame)
	if err != nil {
		s.stop(err)
	}
	return err
}

// letGo gives back the connection's credit for bodies a worker let go of; a
// grant that cannot be written has ended the connection
func (s *session) letGo(n int64) {
	if grant := s.credit.Consume(n); grant > 0 {
		s.stopUnless(s.writer.Send(wire.AppendCredit(nil, 0, uint32(grant)))) //nolint:gosec // within the credit
	}
}

func (s *session) stopUnless(err error) {
	if err != nil {
		s.stop(err)
	}
}

// letGoFromReader is letGo for the reader, which never waits
func (s *session) letGoFromReader(n int64) error {
	if grant := s.credit.Consume(n); grant > 0 {
		return s.post(wire.AppendCredit(nil, 0, uint32(grant))) //nolint:gosec // within the connection's credit
	}
	return nil
}

// finish takes a stream out of use before its final frame leaves, so that the
// client may name its number again as soon as the frame arrives. It closes a
// connection going away once its last stream has finished.
func (s *session) finish(st *stream, frame []byte) error {
	s.mu.Lock()
	delete(s.streams, st.id)
	last := s.goingAway && len(s.streams) == 0
	s.mu.Unlock()
	err := s.send(frame)
	if last {
		s.closeWritten()
	}
	return err
}

// goAway tells the client the server is going: no stream opens after it, and
// the connection closes once the streams running have ended
func (s *session) goAway() {
	s.mu.Lock()
	already := s.goingAway
	s.goingAway = true
	idle := len(s.streams) == 0
	s.mu.Unlock()
	if already {
		return
	}
	goAway := wire.GoAway{Code: wire.CodeUnavailable, Message: errClosing.Error()}
	if err := s.post(wire.AppendFrame(nil, wire.Header{Kind: wire.KindGoAway}, goAway.Append(nil))); err != nil {
		s.stop(err)
		return
	}
	if idle {
		s.closeWritten()
	}
}

// end is what follows the reader's last frame. Every stream waiting for the
// client learns it is gone.
//
// After a clean end, or once the server is going away, the calls running finish
// and their answers are written. Otherwise the calls running end, after the
// GOAWAY saying so if a frame broke the protocol.
func (s *session) end(err error) error {
	s.loseStreams(err)
	if errors.Is(err, io.EOF) || s.isGoingAway() {
		s.workers.stop()
		s.closeWritten()
		return nil
	}
	if errors.Is(err, wire.ErrProtocol) {
		err = errors.Join(err, s.sayLast(wire.GoAway{Code: wire.CodeProtocol, Message: err.Error()}))
	}
	s.stop(err)
	s.workers.stop()
	return err
}

// sayLast writes a GOAWAY as the connection's last frame, and lingers for the
// client to read it
func (s *session) sayLast(goAway wire.GoAway) error {
	deadline := time.AfterFunc(s.server.limits.silence, s.close)
	defer deadline.Stop()
	frame := wire.AppendFrame(nil, wire.Header{Kind: wire.KindGoAway}, goAway.Append(nil))
	if err := s.writer.SendLast(frame); err != nil {
		return err
	}
	return linger(s.conn)
}

func (s *session) isGoingAway() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.goingAway
}

var errLost = errors.New("the client went away")

// loseStreams tells every stream that no DATA, CREDIT or CANCEL will come
func (s *session) loseStreams(cause error) {
	s.mu.Lock()
	s.gone = true
	streams := make([]*stream, 0, len(s.streams))
	for _, st := range s.streams {
		streams = append(streams, st)
	}
	s.mu.Unlock()
	lost := fmt.Errorf("%w: %w", errLost, cause)
	for _, st := range streams {
		st.lose(lost)
	}
}

// closeWritten closes the connection once what is queued is written, or once
// the silence it may take to write it has passed
func (s *session) closeWritten() {
	deadline := time.AfterFunc(s.server.limits.silence, s.close)
	defer deadline.Stop()
	s.stopUnless(s.writer.Flush())
	s.close()
}

// stop ends the connection at once: every stream's context ends with cause
func (s *session) stop(cause error) {
	s.cancel(cause)
	s.close()
}

func (s *session) close() {
	s.closing.Do(func() {
		s.writer.Stop(errLost)
		s.closeErr = unlessClosed(s.conn.Close())
	})
}

// unlessClosed is err, but nil for a connection that was closed already: the
// linger's own deadline, or the client's end
func unlessClosed(err error) error {
	if errors.Is(err, net.ErrClosed) || errors.Is(err, os.ErrClosed) {
		return nil
	}
	return err
}

// watchSilence asks a quiet client with a PING and closes the connection when
// it stays quiet past it; it looks once a silence, not once a frame
func (s *session) watchSilence() *silenceWatch {
	w := &silenceWatch{session: s}
	w.mu.Lock()
	defer w.mu.Unlock()
	w.timer = time.AfterFunc(s.server.limits.silence, w.look)
	return w
}

type silenceWatch struct {
	session *session
	mu      sync.Mutex
	timer   *time.Timer
	last    uint64 // the frames read when it last looked
	pinged  bool
	stopped bool
}

func (w *silenceWatch) look() {
	w.mu.Lock()
	defer w.mu.Unlock()
	s := w.session
	heard := s.heard.Load()
	switch {
	case w.stopped:
		return
	case heard != w.last:
		w.last, w.pinged = heard, false
	case !w.pinged:
		w.pinged = true
		if s.post(wire.AppendFrame(nil, wire.Header{Kind: wire.KindPing}, make([]byte, 8))) != nil {
			return
		}
	default:
		s.stop(errSilent)
		return
	}
	w.timer.Reset(s.server.limits.silence)
}

func (w *silenceWatch) Stop() {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.stopped = true
	w.timer.Stop()
}
