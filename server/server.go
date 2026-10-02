package server

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"maps"
	"net"
	"sync"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/blobs"
	"github.com/tinyshed/tinystore/jobs"
	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/metrics"
	"github.com/tinyshed/tinystore/records"
	"github.com/tinyshed/tinystore/server/wire"
	"github.com/tinyshed/tinystore/sqldb"
)

type Options struct {
	// Version is the server's, as WELCOME states it.
	Version string

	// Logger receives the server's own logs; nil takes the store's, labelled
	// engine=server.
	Logger *slog.Logger

	// Tokens let remote connections in, each with its capability; a local
	// connection needs none.
	Tokens Tokens

	// KV is the kv engine the program opened. When it is nil the server opens
	// it with KVOptions the first time a client asks, since an engine opens
	// once a store. The same holds for every engine below.
	KV             *kv.Store
	KVOptions      kv.Options
	Jobs           *jobs.Store
	JobsOptions    jobs.Options
	Blobs          *blobs.Store
	BlobsOptions   blobs.Options
	Records        *records.Store
	RecordsOptions records.Options
	Metrics        *metrics.Store
	MetricsOptions metrics.Options

	// Stop stops the program serving the store when an admin client asks it
	// to, by server.stop, once the server has answered; nil refuses the
	// request, since a program serving its own store decides when it stops.
	// It returns at once, and the program closes the server as it would.
	Stop func()

	// SQL holds the databases the program opened, by name. A client's sql.open
	// of one checks the migrations it carries against those the file applied,
	// since a database opens once a store. The server opens any other name
	// itself, with the migrations the client carries.
	SQL map[string]*sqldb.DB
}

// Server serves one store to other processes, a sidecar's or a remote
// server's clients alike, through any byte stream.
type Server struct {
	store    *tinystore.Store
	options  Options
	log      *slog.Logger
	instance []byte
	secret   []byte // SERVE names it, and a local WELCOME proves the server holds it
	limits   limits
	methods  map[wire.Method]handler

	opening sync.Mutex // an engine opens once
	kv      *kv.Store
	jobs    *jobs.Store
	blobs   *blobs.Store
	records *records.Store
	metrics *metrics.Store

	sqlOpening sync.Mutex // a database opens once, its migrations applied, while no other engine waits
	databases  map[string]*sqldb.DB

	mu         sync.Mutex
	sessions   map[*session]struct{}
	listeners  map[Listener]struct{}
	closing    bool
	running    sync.WaitGroup
	quietSince time.Time     // when its last connection ended, or it started without one
	comings    chan struct{} // a connection came or went, for WaitIdle
}

// limits bound what a connection holds, docs/server.md's proposals; tests
// shrink the times
type limits struct {
	maxBody          uint32        // the largest body either side sends
	inFlight         uint32        // the streams a client may have open at once
	connectionCredit uint32        // REQUEST and DATA bytes a client sends before credit comes back
	streamCredit     uint32        // DATA bytes a client sends on a stream before credit comes back
	queuedAnswers    int           // answers queued to write, past which a handler waits
	localSessions    int           // connections from this machine at once
	remoteSessions   int           // connections from a network at once
	handshake        time.Duration // how long a connection may take to say HELLO
	silence          time.Duration // quiet before a PING, and again before closing
	statement        time.Duration // how long a data connection's SQL statement runs
}

var defaultLimits = limits{
	maxBody:          1<<20 + 64<<10, // the largest kv or jobs value and the message around it
	inFlight:         256,
	connectionCredit: 8 << 20,
	streamCredit:     2 << 20,
	queuedAnswers:    4 << 20,
	localSessions:    64,
	remoteSessions:   1024,
	handshake:        5 * time.Second,
	silence:          time.Minute,
	statement:        30 * time.Second,
}

// New makes a server of store, which the caller keeps open for as long as it
// serves and closes after it.
func New(store *tinystore.Store, options Options) (*Server, error) {
	instance, secret := make([]byte, wire.InstanceSize), make([]byte, wire.SecretSize)
	if _, err := rand.Read(instance); err != nil {
		return nil, fmt.Errorf("server: an instance: %w", err)
	}
	if _, err := rand.Read(secret); err != nil {
		return nil, fmt.Errorf("server: a secret: %w", err)
	}
	s := &Server{
		store: store, options: options, instance: instance, secret: secret, limits: defaultLimits,
		log: options.Logger, kv: options.KV, jobs: options.Jobs, blobs: options.Blobs, records: options.Records,
		metrics:   options.Metrics,
		databases: maps.Clone(options.SQL), sessions: map[*session]struct{}{}, listeners: map[Listener]struct{}{},
		quietSince: time.Now(), comings: make(chan struct{}, 1),
	}
	if s.databases == nil {
		s.databases = map[string]*sqldb.DB{}
	}
	if s.log == nil {
		s.log = store.Logger("server")
	}
	s.methods = s.handlers()
	return s, nil
}

// Instance is the sixteen random bytes WELCOME repeats, and SERVE names.
func (s *Server) Instance() []byte {
	return append([]byte(nil), s.instance...)
}

var errClosing = errors.New("the server is closing")

// Serve accepts connections from l and serves each. It returns nil when l
// closes, ctx ends or the server closes, and an error when l fails.
//
// The connections it accepted go on until they end, until ctx ends, or until
// Close ends them.
func (s *Server) Serve(ctx context.Context, l Listener) error {
	if !s.track(l) {
		return errClosing
	}
	defer s.untrack(l)
	stop := context.AfterFunc(ctx, func() { _ = l.Close() })
	defer stop()

	for {
		conn, err := l.Accept()
		if err != nil {
			if errors.Is(err, net.ErrClosed) || s.isClosing() || ctx.Err() != nil {
				return nil
			}
			return fmt.Errorf("server: accept on %s: %w", l.Addr(), err)
		}
		go func() {
			if err := s.ServeConn(ctx, conn, l.Remote()); err != nil {
				s.log.Debug("connection ended", "listener", l.Addr(), "error", err)
			}
		}()
	}
}

func (s *Server) track(l Listener) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closing {
		return false
	}
	s.listeners[l] = struct{}{}
	return true
}

func (s *Server) untrack(l Listener) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.listeners, l)
	_ = l.Close()
}

func (s *Server) isClosing() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.closing
}

// ServeConn serves one connection until it ends or ctx ends, as a private child
// serves its parent's stdin and stdout. remote says it came from a network, and
// so it must carry a token.
//
// The end of what the client sends drains the streams running and returns nil.
func (s *Server) ServeConn(ctx context.Context, conn io.ReadWriteCloser, remote bool) error {
	session, err := s.join(ctx, conn, remote)
	if err != nil {
		return errors.Join(err, s.refuse(conn, err))
	}
	defer s.leave(session)
	return session.serve()
}

// join counts a connection in, or refuses it past its kind's bound or once the
// server is closing
func (s *Server) join(ctx context.Context, conn io.ReadWriteCloser, remote bool) (*session, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closing {
		return nil, errClosing
	}
	most, same := s.limits.localSessions, 0
	if remote {
		most = s.limits.remoteSessions
	}
	for joined := range s.sessions {
		if joined.remote == remote {
			same++
		}
	}
	if same >= most {
		return nil, fmt.Errorf("%w: %d connections of its kind already", errTooMany, same)
	}
	session := newSession(ctx, s, conn, remote)
	s.sessions[session] = struct{}{}
	s.running.Add(1)
	s.cameOrWent()
	return session, nil
}

var errTooMany = errors.New("too many connections")

func (s *Server) leave(session *session) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.sessions, session)
	if len(s.sessions) == 0 {
		s.quietSince = time.Now()
	}
	s.running.Done()
	s.cameOrWent()
}

func (s *Server) cameOrWent() {
	select {
	case s.comings <- struct{}{}:
	default:
	}
}

// WaitIdle returns once no connection has been open for idle, as a shared
// sidecar leaves when its clients have. It returns ctx's error if ctx ends
// first.
func (s *Server) WaitIdle(ctx context.Context, idle time.Duration) error {
	for {
		s.mu.Lock()
		open, since := len(s.sessions), s.quietSince
		s.mu.Unlock()
		wait := idle
		if open == 0 {
			if wait = idle - time.Since(since); wait <= 0 {
				return nil
			}
		}
		timer := time.NewTimer(wait)
		select {
		case <-ctx.Done():
			timer.Stop()
			return ctx.Err()
		case <-s.comings:
			timer.Stop()
		case <-timer.C:
		}
	}
}

// refuse answers a connection the server does not take: it reads the HELLO,
// so that the client finds the GOAWAY where its WELCOME belongs, and closes
func (s *Server) refuse(conn io.ReadWriteCloser, why error) error {
	timeout := time.AfterFunc(s.limits.handshake, func() { _ = conn.Close() })
	defer timeout.Stop()
	defer conn.Close()
	r := wire.NewReader(conn, s.limits.maxBody)
	if _, err := r.Header(); err != nil {
		return err
	}
	if _, err := r.Body(nil); err != nil {
		return err
	}
	code := wire.CodeUnavailable
	if errors.Is(why, errTooMany) {
		code = wire.CodeLimit
	}
	goAway := wire.GoAway{Code: code, Message: why.Error()}
	if _, err := conn.Write(wire.AppendFrame(nil, wire.Header{Kind: wire.KindGoAway}, goAway.Append(nil))); err != nil {
		return err
	}
	return linger(conn)
}

// linger lets a GOAWAY reach a client that may still be sending: closing a TCP
// connection with bytes unread resets it, and a reset may discard the GOAWAY
// before the client reads it. It closes the writing side and reads what
// arrives until the client closes, a second at most.
func linger(conn io.ReadWriteCloser) error {
	half, ok := conn.(interface{ CloseWrite() error })
	if !ok {
		return nil
	}
	if err := half.CloseWrite(); err != nil {
		return unlessClosed(err)
	}
	timeout := time.AfterFunc(time.Second, func() { _ = conn.Close() })
	defer timeout.Stop()
	_, err := io.Copy(io.Discard, conn)
	return unlessClosed(err)
}

// Close stops the listeners and tells every connection with a GOAWAY that the
// server is going: the streams running finish, a REQUEST that crossed the
// GOAWAY is answered unavailable, and each connection closes when its last
// stream ends. When ctx ends first the streams still running are cancelled.
func (s *Server) Close(ctx context.Context) error {
	s.mu.Lock()
	s.closing = true
	listeners := make([]Listener, 0, len(s.listeners))
	for l := range s.listeners {
		listeners = append(listeners, l)
	}
	sessions := make([]*session, 0, len(s.sessions))
	for joined := range s.sessions {
		sessions = append(sessions, joined)
	}
	s.mu.Unlock()

	for _, l := range listeners {
		_ = l.Close()
	}
	for _, joined := range sessions {
		joined.goAway()
	}

	done := make(chan struct{})
	go func() {
		s.running.Wait()
		close(done)
	}()
	select {
	case <-done:
		return nil
	case <-ctx.Done():
		for _, joined := range sessions {
			joined.stop(errClosing)
		}
		<-done
		return ctx.Err()
	}
}

// kvStore is the kv engine, opened the first time a client asks for it
func (s *Server) kvStore(ctx context.Context) (*kv.Store, error) {
	s.opening.Lock()
	defer s.opening.Unlock()
	if s.kv == nil {
		opened, err := kv.Open(ctx, s.store, s.options.KVOptions)
		if err != nil {
			return nil, err
		}
		s.kv = opened
	}
	return s.kv, nil
}

func (s *Server) jobsStore(ctx context.Context) (*jobs.Store, error) {
	s.opening.Lock()
	defer s.opening.Unlock()
	if s.jobs == nil {
		opened, err := jobs.Open(ctx, s.store, s.options.JobsOptions)
		if err != nil {
			return nil, err
		}
		s.jobs = opened
	}
	return s.jobs, nil
}

func (s *Server) blobsStore(ctx context.Context) (*blobs.Store, error) {
	s.opening.Lock()
	defer s.opening.Unlock()
	if s.blobs == nil {
		opened, err := blobs.Open(ctx, s.store, s.options.BlobsOptions)
		if err != nil {
			return nil, err
		}
		s.blobs = opened
	}
	return s.blobs, nil
}

func (s *Server) recordsStore(ctx context.Context) (*records.Store, error) {
	s.opening.Lock()
	defer s.opening.Unlock()
	if s.records == nil {
		opened, err := records.Open(ctx, s.store, s.options.RecordsOptions)
		if err != nil {
			return nil, err
		}
		s.records = opened
	}
	return s.records, nil
}

func (s *Server) metricsStore(ctx context.Context) (*metrics.Store, error) {
	s.opening.Lock()
	defer s.opening.Unlock()
	if s.metrics == nil {
		opened, err := metrics.Open(ctx, s.store, s.options.MetricsOptions)
		if err != nil {
			return nil, err
		}
		s.metrics = opened
	}
	return s.metrics, nil
}

// engines is what WELCOME says this server serves
func (s *Server) engines() []string {
	return []string{"kv", "jobs", "blobs", "sql", "records", "metrics"}
}
