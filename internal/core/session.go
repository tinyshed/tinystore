package core

import (
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/binary"
	"sync"

	"github.com/tinyshed/tinystore/internal/wire"
)

// A session is a connection's protocol, without its bytes' way there and
// back: frames in, frames out, streams, credit both ways, PING, GOAWAY and
// cancelling, as docs/wire.md says them. The transport hands it what it read
// and takes what it writes, so the same code serves the core in this process,
// a Unix socket, a Windows pipe, a child's stdio and TLS.
//
// One mutex guards the session and its streams. Nothing holds it while it
// waits: a frame to write is queued for the writer goroutine, which gathers
// what was asked for meanwhile into one write.

// ProtocolVersion is the protocol this SDK speaks.
const ProtocolVersion = 2

// DownloadWindow is what this client lets the server send on a stream
// before it grants more: a body at least.
const DownloadWindow = 2 << 20

// welcomeMost is the largest body a WELCOME may come in, before a body is agreed.
const welcomeMost = 1 << 20

// Agreed is what a WELCOME agreed.
type Agreed struct {
	Server           string
	Protocol         uint64
	Instance         []byte
	Capability       string
	MaxBody          uint64
	InFlight         uint64
	ConnectionCredit uint64
	StreamCredit     uint64
	Engines          []string
	Now              int64
	// Durability is how far the store's files' commits go, when it was opened with one: full or os.
	Durability string
}

// Event is a frame of a stream's answer: the RESPONSE or a DATA, End saying it is the last.
type Event struct {
	Response bool
	Body     []byte
	End      bool
}

// Session is the protocol of one connection.
type Session struct {
	mu        sync.Mutex
	write     func([]byte) error
	secret    []byte
	challenge []byte
	welcomed  chan struct{}
	agreed    *Agreed
	streams   map[uint32]*Stream
	next      uint32
	queue     []*queued
	credit    uint64
	rest      []byte
	going     bool
	ended     error
	waiting   []chan struct{}
	onEnd     []func(error)
	out       [][]byte
	wake      chan struct{}
	stopped   chan struct{}
}

// queued is a frame waiting for the connection's credit, in the order it was asked for.
type queued struct {
	stream  *Stream
	bytes   []byte
	counted uint64
	request bool
}

// Hello is what a client says first: who it is, a remote server's token,
// and the challenge a server found through SERVE proves it read SERVE with.
type Hello struct {
	Client    string
	Token     string
	Secret    []byte
	Challenge []byte
}

// NewSession starts a session that writes through write, sending its HELLO.
func NewSession(write func([]byte) error, hello Hello) *Session {
	s := &Session{
		write:     write,
		secret:    hello.Secret,
		challenge: hello.Challenge,
		welcomed:  make(chan struct{}),
		streams:   map[uint32]*Stream{},
		wake:      make(chan struct{}, 1),
		stopped:   make(chan struct{}),
	}
	message := wire.Hello{Protocol: ProtocolVersion, Client: hello.Client, StreamCredit: ptr(uint64(DownloadWindow))}
	if hello.Token != "" {
		message.Token = &hello.Token
	}
	if hello.Challenge != nil {
		message.Challenge = &hello.Challenge
	}
	go s.writer()
	s.mu.Lock()
	s.post(frame(kindHello, 0, 0, 0, wire.Encode(&message)))
	s.mu.Unlock()
	return s
}

func ptr[T any](v T) *T { return &v }

// Welcomed waits for the WELCOME, and gives what it agreed.
func (s *Session) Welcomed(ctx context.Context) (*Agreed, error) {
	select {
	case <-s.welcomed:
	case <-ctx.Done():
		return nil, ctx.Err()
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.agreed == nil {
		return nil, s.ended
	}
	return s.agreed, nil
}

// Agreed is what the WELCOME agreed, nil before it came.
func (s *Session) Agreed() *Agreed {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.agreed
}

// Ended is why the session ended, nil while it runs.
func (s *Session) Ended() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.ended
}

// GoingAway says whether the server is closing the connection: no stream opens on it.
func (s *Session) GoingAway() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.going
}

// OnEnd calls listener once the session ends, at once when it has.
func (s *Session) OnEnd(listener func(error)) {
	s.mu.Lock()
	ended := s.ended
	if ended == nil {
		s.onEnd = append(s.onEnd, listener)
	}
	s.mu.Unlock()
	if ended != nil {
		listener(ended)
	}
}

// Open opens a stream with its REQUEST, waiting while every stream in flight
// is in use; end says the REQUEST is the client's whole side, an upload's
// DATA following through the stream otherwise.
func (s *Session) Open(ctx context.Context, method uint16, body []byte, end bool) (*Stream, error) {
	agreed, err := s.Welcomed(ctx)
	if err != nil {
		return nil, err
	}
	if uint64(len(body)) > agreed.MaxBody {
		return nil, Errorf(Limit, "a request of %d bytes, past the %d a body holds", len(body), agreed.MaxBody)
	}
	s.mu.Lock()
	for uint64(len(s.streams)) >= agreed.InFlight && s.refused() == nil {
		slot := make(chan struct{})
		s.waiting = append(s.waiting, slot)
		s.mu.Unlock()
		select {
		case <-slot:
		case <-ctx.Done():
			return nil, ctx.Err()
		}
		s.mu.Lock()
	}
	if err := s.refused(); err != nil {
		s.mu.Unlock()
		return nil, err
	}
	st := &Stream{
		s:            s,
		ID:           s.nextID(),
		Method:       method,
		ready:        make(chan struct{}, 1),
		credited:     make(chan struct{}, 1),
		uploadCredit: agreed.StreamCredit,
	}
	s.streams[st.ID] = st
	var flags uint8
	if end {
		flags = flagEnd
	}
	s.enqueue(&queued{st, frame(kindRequest, flags, method, st.ID, body), uint64(len(body)), true})
	s.mu.Unlock()
	return st, nil
}

// Call is a call: one REQUEST, and the RESPONSE that answers it. A ctx done
// while it waits cancels the stream.
func (s *Session) Call(ctx context.Context, method uint16, body []byte) ([]byte, error) {
	st, err := s.Open(ctx, method, body, true)
	if err != nil {
		return nil, err
	}
	event, err := st.Next(ctx)
	if err != nil {
		return nil, err
	}
	if !event.Response || !event.End {
		s.Fault(Errorf(Protocol, "a DATA or a RESPONSE without END answering a call on stream %d", st.ID))
	}
	return event.Body, nil
}

// refused is why no stream may open now: the connection ended or is going away.
func (s *Session) refused() error {
	if s.ended != nil {
		return lost(s.ended, false)
	}
	if s.going {
		return Errorf(Unavailable, "the server is closing this connection")
	}
	return nil
}

// nextID numbers a stream from 1, never 0 and never one in use, so that a
// frame for a stream that has ended never reaches the next one.
func (s *Session) nextID() uint32 {
	for {
		s.next++
		if s.next == 0 {
			s.next = 1
		}
		if _, used := s.streams[s.next]; !used {
			return s.next
		}
	}
}

func (s *Session) enqueue(q *queued) {
	s.queue = append(s.queue, q)
	s.drain()
}

// drain sends the frames that count against the connection's credit in the
// order they were asked for, as far as the credit goes.
func (s *Session) drain() {
	for len(s.queue) > 0 {
		q := s.queue[0]
		if q.counted > s.credit {
			return
		}
		s.queue = s.queue[1:]
		s.credit -= q.counted
		if q.request {
			q.stream.sent = true
		}
		s.post(q.bytes)
	}
}

// post queues a frame that does not wait for credit: a CREDIT, a CANCEL, a PONG.
func (s *Session) post(f []byte) {
	if s.ended != nil {
		return
	}
	s.out = append(s.out, f)
	select {
	case s.wake <- struct{}{}:
	default:
	}
}

// writer writes what was posted, every frame asked for meanwhile in one write.
func (s *Session) writer() {
	for {
		select {
		case <-s.wake:
		case <-s.stopped:
			return
		}
		s.mu.Lock()
		out := s.out
		s.out = nil
		s.mu.Unlock()
		if len(out) == 0 {
			continue
		}
		joined := out[0]
		if len(out) > 1 {
			size := 0
			for _, f := range out {
				size += len(f)
			}
			joined = make([]byte, 0, size)
			for _, f := range out {
				joined = append(joined, f...)
			}
		}
		if err := s.write(joined); err != nil {
			s.End(Wrap(Closed, err, "cannot write to the server"))
			return
		}
	}
}

func (s *Session) cancel(st *Stream) {
	if !st.sent {
		s.dropQueued(st, true)
		s.release(st)
		return
	}
	s.post(frame(kindCancel, 0, 0, st.ID, nil))
}

func (s *Session) dropQueued(st *Stream, requests bool) {
	kept := s.queue[:0]
	for _, q := range s.queue {
		if q.stream != st || (q.request && !requests) {
			kept = append(kept, q)
		}
	}
	s.queue = kept
}

// release takes a stream out of use, dropping what of it has not left: its
// number may be named again at once.
func (s *Session) release(st *Stream) {
	st.finished = true
	st.signalCredit()
	if s.streams[st.ID] == st {
		delete(s.streams, st.ID)
	}
	s.dropQueued(st, false)
	if len(s.waiting) > 0 {
		close(s.waiting[0])
		s.waiting = s.waiting[1:]
	}
}

// Receive takes bytes the transport read: any part of a frame, or several.
func (s *Session) Receive(chunk []byte) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ended != nil {
		return
	}
	data := chunk
	if len(s.rest) > 0 {
		data = append(s.rest, chunk...)
		s.rest = nil
	}
	at := 0
	for len(data)-at >= headerSize {
		h := parseHeader(data[at:])
		most := uint64(welcomeMost)
		if s.agreed != nil {
			most = s.agreed.MaxBody
		}
		if err := h.check(most); err != nil {
			s.end(err)
			return
		}
		stop := at + headerSize + int(h.length)
		if stop > len(data) {
			break
		}
		body := make([]byte, h.length)
		copy(body, data[at+headerSize:stop])
		if err := s.take(h, body); err != nil {
			s.end(err)
			return
		}
		at = stop
	}
	if at < len(data) {
		s.rest = append([]byte(nil), data[at:]...)
	}
}

func (s *Session) take(h header, body []byte) error {
	if s.agreed == nil {
		return s.handshake(h.kind, body)
	}
	switch h.kind {
	case kindResponse, kindData:
		return s.answer(h.kind, h.flags, h.stream, body)
	case kindCredit:
		s.credited(h.stream, uint64(binary.LittleEndian.Uint32(body)))
	case kindPing:
		s.post(frame(kindPong, 0, 0, 0, body))
	case kindPong:
	case kindGoAway:
		return s.goAway(body)
	default:
		return Errorf(Protocol, "a %s from the server", kindName(h.kind))
	}
	return nil
}

func (s *Session) handshake(kind uint8, body []byte) error {
	if kind == kindGoAway {
		var away wire.GoAway
		if err := wire.Decode(body, &away); err != nil {
			return Wrap(Protocol, err, "a GOAWAY that does not read")
		}
		s.end(Failed(away.Code, away.Message, nil))
		return nil
	}
	if kind != kindWelcome {
		return Errorf(Protocol, "a %s where the WELCOME belongs", kindName(kind))
	}
	var w wire.Welcome
	if err := wire.Decode(body, &w); err != nil {
		return Wrap(Protocol, err, "a WELCOME that does not read")
	}
	if w.Protocol != ProtocolVersion {
		return Errorf(Protocol, "the server speaks protocol %d, not %d", w.Protocol, ProtocolVersion)
	}
	if s.secret != nil && s.challenge != nil && !proves(s.secret, s.challenge, w.Proof) {
		return Errorf(Protocol, "the server cannot prove it read SERVE: another process holds its endpoint")
	}
	s.agreed = &Agreed{
		Server: w.Server, Protocol: w.Protocol, Instance: w.Instance, Capability: w.Capability,
		MaxBody: w.MaxBody, InFlight: max(w.InFlight, 1), ConnectionCredit: w.ConnectionCredit,
		StreamCredit: w.StreamCredit, Engines: w.Engines, Now: w.Now,
	}
	if w.Durability != nil {
		s.agreed.Durability = *w.Durability
	}
	s.credit = s.agreed.ConnectionCredit
	close(s.welcomed)
	return nil
}

func (s *Session) answer(kind, flags uint8, number uint32, body []byte) error {
	st, ok := s.streams[number]
	if !ok {
		return Errorf(Protocol, "a %s on stream %d, which is not in use", kindName(kind), number)
	}
	end := flags&flagEnd != 0
	if end {
		s.release(st)
	}
	if flags&flagError != 0 {
		st.fail(failureOf(body))
		return nil
	}
	st.deliver(Event{Response: kind == kindResponse, Body: body, End: end})
	if st.cancelled && kind == kindData {
		st.consumed(uint64(len(body)))
	}
	return nil
}

func (s *Session) credited(number uint32, n uint64) {
	if number == 0 {
		s.credit += n
		s.drain()
		return
	}
	if st, ok := s.streams[number]; ok {
		st.uploadCredit += n
		st.signalCredit()
	}
}

// goAway takes a GOAWAY: no stream opens after it, and a REQUEST still
// waiting for credit goes nowhere and may be sent on another connection.
func (s *Session) goAway(body []byte) error {
	var away wire.GoAway
	if err := wire.Decode(body, &away); err != nil {
		return Wrap(Protocol, err, "a GOAWAY that does not read")
	}
	s.going = true
	message := away.Message
	if message == "" {
		message = "the server is closing this connection"
	}
	unsent := Errorf(Unavailable, "%s", message)
	for _, q := range append([]*queued(nil), s.queue...) {
		if q.request {
			q.stream.fail(unsent)
			s.release(q.stream)
		}
	}
	for _, slot := range s.waiting {
		close(slot)
	}
	s.waiting = nil
	return nil
}

// Fault ends the session with a frame that broke the protocol.
func (s *Session) Fault(err error) {
	s.End(err)
}

// End ends the session; every stream still open learns whether its REQUEST had left.
func (s *Session) End(err error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.end(err)
}

// end ends the session under its lock; the listeners run on a goroutine of
// their own, since they close what the lock's holder may be reading from.
func (s *Session) end(err error) {
	if s.ended != nil {
		return
	}
	s.ended = err
	select {
	case <-s.welcomed:
	default:
		close(s.welcomed)
	}
	for _, st := range s.streams {
		st.fail(lost(err, st.sent))
		st.finished = true
		st.signalCredit()
	}
	s.streams = map[uint32]*Stream{}
	s.queue = nil
	for _, slot := range s.waiting {
		close(slot)
	}
	s.waiting = nil
	close(s.stopped)
	listeners := s.onEnd
	s.onEnd = nil
	if len(listeners) > 0 {
		go func() {
			for _, listener := range listeners {
				listener(err)
			}
		}()
	}
}

// Streams counts the streams in use, their final frames not yet come.
func (s *Session) Streams() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.streams)
}

// failureOf is the error a final frame with ERROR carries.
func failureOf(body []byte) error {
	var failure wire.Failure
	if err := wire.Decode(body, &failure); err != nil {
		return Wrap(Protocol, err, "a failure that does not read")
	}
	var what map[string]string
	if failure.What != nil {
		what = *failure.What
	}
	return Failed(failure.Code, failure.Message, what)
}

// proves says whether a WELCOME's proof is the HMAC-SHA256 of the challenge
// keyed with SERVE's secret.
func proves(secret, challenge []byte, proof *[]byte) bool {
	if proof == nil || len(*proof) != 32 {
		return false
	}
	mac := hmac.New(sha256.New, secret)
	mac.Write(challenge)
	return hmac.Equal(mac.Sum(nil), *proof)
}

// Stream is a client's REQUEST from its frame to the server's final frame:
// the span in which its number is in use.
type Stream struct {
	s      *Session
	ID     uint32
	Method uint16
	// sent says the REQUEST has left, so the server may have acted on it
	sent         bool
	finished     bool
	cancelled    bool
	events       []Event
	ready        chan struct{}
	failure      error
	uploadCredit uint64
	credited     chan struct{}
	granting     uint64
}

// Next is the next frame of the answer, or its error; events already
// arrived are read first. A ctx done while it waits cancels the stream.
func (st *Stream) Next(ctx context.Context) (Event, error) {
	for {
		st.s.mu.Lock()
		if len(st.events) > 0 {
			event := st.events[0]
			st.events = st.events[1:]
			st.s.mu.Unlock()
			return event, nil
		}
		if st.failure != nil {
			failure := st.failure
			st.s.mu.Unlock()
			return Event{}, failure
		}
		st.s.mu.Unlock()
		select {
		case <-st.ready:
		case <-ctx.Done():
			st.Cancel()
			return Event{}, ctx.Err()
		}
	}
}

// Send sends a body of an upload as DATA once the stream's credit lets it
// go; end says it is the last.
func (st *Stream) Send(ctx context.Context, body []byte, end bool) error {
	st.s.mu.Lock()
	for !st.finished && st.uploadCredit < uint64(len(body)) {
		st.s.mu.Unlock()
		select {
		case <-st.credited:
		case <-ctx.Done():
			st.Cancel()
			return ctx.Err()
		}
		st.s.mu.Lock()
	}
	defer st.s.mu.Unlock()
	if st.finished {
		if st.failure != nil {
			return st.failure
		}
		return Errorf(Closed, "the stream ended before its upload did")
	}
	st.uploadCredit -= uint64(len(body))
	var flags uint8
	if end {
		flags = flagEnd
	}
	st.s.enqueue(&queued{st, frame(kindData, flags, 0, st.ID, body), uint64(len(body)), false})
	return nil
}

// Consumed gives back what a DATA took of the stream's credit once the
// caller is done with it; a grant goes once half the window has.
func (st *Stream) Consumed(n int) {
	st.s.mu.Lock()
	defer st.s.mu.Unlock()
	st.consumed(uint64(n))
}

func (st *Stream) consumed(n uint64) {
	if st.finished {
		return
	}
	st.granting += n
	if st.granting >= DownloadWindow/2 {
		st.s.post(credit(st.ID, st.granting))
		st.granting = 0
	}
}

// Cancel asks the server to stop. A stream whose REQUEST has not left never
// leaves; one that has keeps its number until the server's final frame,
// which is dropped.
func (st *Stream) Cancel() {
	st.s.mu.Lock()
	defer st.s.mu.Unlock()
	if st.finished || st.cancelled {
		return
	}
	st.cancelled = true
	st.s.cancel(st)
	st.fail(Errorf(Cancelled, "the call was cancelled"))
}

// Finished says the server's final frame came, or the connection ended.
func (st *Stream) Finished() bool {
	st.s.mu.Lock()
	defer st.s.mu.Unlock()
	return st.finished
}

// Sent says the REQUEST has left.
func (st *Stream) Sent() bool {
	st.s.mu.Lock()
	defer st.s.mu.Unlock()
	return st.sent
}

func (st *Stream) deliver(event Event) {
	if st.cancelled {
		return
	}
	st.events = append(st.events, event)
	st.signal()
}

// fail ends what waits on the stream with err; events already arrived are read first.
func (st *Stream) fail(err error) {
	if st.failure == nil {
		st.failure = err
	}
	st.signal()
	st.signalCredit()
}

func (st *Stream) signal() {
	select {
	case st.ready <- struct{}{}:
	default:
	}
}

func (st *Stream) signalCredit() {
	select {
	case st.credited <- struct{}{}:
	default:
	}
}
