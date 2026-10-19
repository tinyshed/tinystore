package core

import (
	"context"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"

	"tinyshed.org/tinystore/internal/wire"
)

// The core in this process, through its pipe: the four C functions of
// plan/ffi.md over the library crates/ffi builds. The frames go in from any
// goroutine with no room for an answer, and come out on one goroutine of the
// connection's own, which waits in recv; nothing the core does waits for Go.

// readWait is how long the reading goroutine waits in one recv, in
// milliseconds; the end of the connection wakes it at once.
const readWait = 1000

// core is the library's four functions, as the platform reaches them.
type coreLibrary interface {
	open(dir, options []byte) (uintptr, error)
	send(conn uintptr, frames []byte)
	recv(conn uintptr, waitMs uint32, into []byte) int
	close(conn uintptr)
}

var (
	librariesMu sync.Mutex
	libraries   = map[string]coreLibrary{}
)

// FindLibrary is the core's library: the one given, or TINYSTORE_LIBRARY.
func FindLibrary(given string) (string, error) {
	if given != "" {
		return given, nil
	}
	if found := os.Getenv("TINYSTORE_LIBRARY"); found != "" {
		return found, nil
	}
	return "", Errorf(Closed, "no core library for the store in this process: pass tinystore.Library, or set TINYSTORE_LIBRARY")
}

func libraryAt(path string) (coreLibrary, error) {
	librariesMu.Lock()
	defer librariesMu.Unlock()
	if found, ok := libraries[path]; ok {
		return found, nil
	}
	loaded, err := loadLibrary(path)
	if err != nil {
		return nil, Wrap(Closed, err, "cannot load the core from %s", path)
	}
	libraries[path] = loaded
	return loaded, nil
}

// InProcess dials the core in this process: no server and no socket.
func InProcess(directory, library string, started Started) func(context.Context) (*Connection, error) {
	absolute, _ := filepath.Abs(directory)
	options := wire.StoreOptions{SelfMetrics: started.SelfMetrics}
	if started.Durability != "" {
		options.Durability = &started.Durability
	}
	if started.EncryptionKeyFile != "" {
		key, _ := filepath.Abs(started.EncryptionKeyFile)
		options.EncryptionKeyFile = &key
	}
	encoded := wire.Encode(&options)
	return func(ctx context.Context) (*Connection, error) {
		path, err := FindLibrary(library)
		if err != nil {
			return nil, err
		}
		lib, err := libraryAt(path)
		if err != nil {
			return nil, err
		}
		conn, err := lib.open([]byte(absolute), encoded)
		if err != nil {
			return nil, err
		}
		p := &pipe{lib: lib, conn: conn, read: make(chan struct{})}
		session := NewSession(p.write, Hello{Client: Client})
		go p.reading(session)
		c := NewConnection(session, p, p.letGo)
		if err := c.Handshake(ctx); err != nil {
			p.letGo()
			return nil, err
		}
		return c, nil
	}
}

// pipe is a connection to the core in this process.
type pipe struct {
	lib     coreLibrary
	conn    uintptr
	mu      sync.Mutex
	closing atomic.Bool
	read    chan struct{}
	once    sync.Once
}

func (p *pipe) write(frames []byte) error {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.closing.Load() {
		return Errorf(Closed, "the store closed")
	}
	p.lib.send(p.conn, frames)
	return nil
}

func (p *pipe) reading(session *Session) {
	defer close(p.read)
	into := make([]byte, 1<<20)
	for !p.closing.Load() {
		n := p.lib.recv(p.conn, readWait, into)
		if n > 0 {
			session.Receive(into[:n])
		}
	}
	session.End(Errorf(Closed, "the store closed"))
}

func (p *pipe) Write(frames []byte) error { return p.write(frames) }

// Close ends the session with the client's GOAWAY, which wakes the goroutine in recv.
func (p *pipe) Close() error {
	p.mu.Lock()
	defer p.mu.Unlock()
	if !p.closing.Load() {
		away := wire.GoAway{Code: "closed", Message: "the store closed"}
		p.lib.send(p.conn, frame(kindGoAway, 0, 0, 0, wire.Encode(&away)))
		p.closing.Store(true)
	}
	return nil
}

// letGo closes the connection once its reader has stopped, so that the core
// lets the directory go before the store's close returns.
func (p *pipe) letGo() {
	_ = p.Close()
	p.once.Do(func() {
		<-p.read
		p.lib.close(p.conn)
	})
}
