package server

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/server/internal/private"
	"github.com/tinyshed/tinystore/server/wire"
)

// What a local server publishes in its store's directory, docs/wire.md "Finding a local server".
const (
	publishedDir = "server/"
	serveName    = "SERVE"
	socketName   = "tinystore.sock"
)

// Publish listens where the directory's shared sidecar is found and writes
// SERVE beside it, whole. The place is a Unix socket in <dir>/server/ or, on
// Windows, a named pipe named after the directory.
//
// The store holds the directory's lock, so claiming server/ from it proves the
// directory is this server's to publish. The directory is its owner's alone.
//
// unpublish removes SERVE and the socket. Serve closes the listener once the
// server closes.
func (s *Server) Publish(ctx context.Context) (l Listener, unpublish func() error, err error) {
	dir, release, err := s.store.Claim(publishedDir)
	if err != nil {
		return nil, nil, fmt.Errorf("server: publish: %w", err)
	}
	defer func() {
		if err != nil {
			release()
		}
	}()
	if err = private.Dir(dir); err != nil {
		return nil, nil, fmt.Errorf("server: publish: %w", err)
	}
	// a SERVE left behind names an endpoint another process may have taken
	// since, so it goes before this server tries to listen
	if err = removeServe(dir); err != nil {
		return nil, nil, fmt.Errorf("server: publish: %w", err)
	}
	endpoint, err := localEndpoint(dir)
	if err != nil {
		return nil, nil, fmt.Errorf("server: publish: %w", err)
	}
	if l, err = listenLocal(ctx, endpoint); err != nil {
		return nil, nil, fmt.Errorf("server: publish: %w", err)
	}
	if err = s.writeServe(dir, l.Addr()); err != nil {
		return nil, nil, errors.Join(fmt.Errorf("server: publish: %w", err), l.Close())
	}
	return l, func() error {
		defer release()
		return errors.Join(removeServe(dir), removeSocket(endpoint))
	}, nil
}

// Share serves a program's own store to the other processes of its machine
// while the program runs: it publishes the store as Publish does and serves
// whoever finds it there, the tinystore command, an agent over MCP, and Bun
// and Python programs alike. options carries the engines the program opened,
// since each opens once a store.
//
//	stop, err := server.Share(ctx, store, server.Options{KV: state})
//	if err != nil {
//		return err
//	}
//	defer stop()
//
// ctx bounds publishing alone. stop removes SERVE and closes the server, giving
// the streams running ten seconds to finish before it cancels them, and waits
// for every connection to end; it comes before the store's Close.
func Share(ctx context.Context, store *tinystore.Store, options Options) (stop func() error, err error) {
	srv, err := New(store, options)
	if err != nil {
		return nil, err
	}
	l, unpublish, err := srv.Publish(ctx)
	if err != nil {
		return nil, err
	}
	served := make(chan error, 1)
	go func() { served <- srv.Serve(context.WithoutCancel(ctx), l) }()

	var once sync.Once
	var stopped error
	return func() error {
		once.Do(func() {
			closing, cancel := context.WithTimeout(context.WithoutCancel(ctx), stopWait)
			defer cancel()
			stopped = errors.Join(unpublish(), srv.Close(closing))
			if servedErr := <-served; !errors.Is(servedErr, errClosing) {
				stopped = errors.Join(stopped, servedErr)
			}
			_ = l.Close() // a Serve that found the server closing never took the listener
		})
		return stopped
	}, nil
}

// the time stop gives a shared store's streams before it cancels them, as a
// sidecar gives its own
const stopWait = 10 * time.Second

// writeServe writes SERVE as a file of its own and renames it into place, so
// that a client reads the whole of it or nothing
func (s *Server) writeServe(dir string, endpoints ...string) error {
	text, err := json.Marshal(wire.Published{
		Protocol: wire.Protocol, Server: s.options.Version, PID: os.Getpid(),
		Instance: base64.RawURLEncoding.EncodeToString(s.instance),
		Secret:   base64.RawURLEncoding.EncodeToString(s.secret), Endpoints: endpoints, Sidecar: s.options.Sidecar,
	})
	if err != nil {
		return err
	}
	file, err := os.CreateTemp(dir, serveName+".*")
	if err != nil {
		return err
	}
	_, err = file.Write(append(text, '\n'))
	if err = errors.Join(err, file.Sync(), file.Close()); err != nil {
		return errors.Join(err, os.Remove(file.Name()))
	}
	if err = whileHeld(func() error { return os.Rename(file.Name(), filepath.Join(dir, serveName)) }); err != nil {
		return errors.Join(err, os.Remove(file.Name()))
	}
	return nil
}

func removeServe(dir string) error {
	return unlessMissing(whileHeld(func() error { return os.Remove(filepath.Join(dir, serveName)) }))
}

// the tries of a change to SERVE that Windows refuses while another handle
// holds the file, 1 to 64 ms apart
const heldTries = 8

// ERROR_ACCESS_DENIED and ERROR_SHARING_VIOLATION: what Windows says of a file
// another handle holds without sharing its deletion
const (
	errorAccessDenied     syscall.Errno = 5
	errorSharingViolation syscall.Errno = 32
)

// whileHeld tries a rename or a remove of SERVE again for a moment on Windows.
// Windows refuses both while a client reads SERVE, which Go and Python open
// without sharing its deletion, or while a scanner reads it.
func whileHeld(change func() error) error {
	for try := 1; ; try++ {
		err := change()
		held := errors.Is(err, errorAccessDenied) || errors.Is(err, errorSharingViolation)
		if !held || runtime.GOOS != "windows" || try == heldTries {
			return err
		}
		time.Sleep(time.Millisecond << (try - 1))
	}
}

// localEndpoint is where a store's shared sidecar listens.
//
// On Windows it is a named pipe named after the store's absolute path.
// Elsewhere it is a socket in server/ when its path fits a sockaddr_un, and
// otherwise a socket in a directory of the user's own, named after the store's
// path.
func localEndpoint(dir string) (string, error) {
	absolute, err := filepath.Abs(dir)
	if err != nil {
		return "", err
	}
	// eight bytes, so that a socket under macOS's temporary directory still fits
	named := sha256.Sum256([]byte(filepath.Dir(absolute)))
	name := "tinystore-" + hex.EncodeToString(named[:8])
	if runtime.GOOS == "windows" {
		return "pipe:" + name, nil
	}
	socket := filepath.Join(absolute, socketName)
	if len(socket) > socketPathMost() {
		runtimeDir := os.Getenv("XDG_RUNTIME_DIR")
		if runtimeDir == "" {
			runtimeDir = os.TempDir()
		}
		own := filepath.Join(runtimeDir, name)
		if err = private.Dir(own); err != nil {
			return "", err
		}
		socket = filepath.Join(own, socketName)
	}
	if most := socketPathMost(); len(socket) > most {
		return "", fmt.Errorf("a socket path of %d bytes, past the %d a sockaddr_un holds", len(socket), most)
	}
	return "unix://" + socket, nil
}

// socketPathMost is the bytes a Unix socket's path holds, its NUL apart: 104
// on macOS and the BSDs, 108 on Linux
func socketPathMost() int {
	if runtime.GOOS == "darwin" || runtime.GOOS == "ios" || strings.HasSuffix(runtime.GOOS, "bsd") {
		return 103
	}
	return 107
}

// listenLocal listens at a local endpoint, removing the socket a server
// before it left: the store's lock says that server is gone
func listenLocal(ctx context.Context, endpoint string) (Listener, error) {
	if err := removeSocket(endpoint); err != nil {
		return nil, err
	}
	return Listen(ctx, endpoint, nil)
}

func removeSocket(endpoint string) error {
	if path, isSocket := strings.CutPrefix(endpoint, "unix://"); isSocket {
		return unlessMissing(os.Remove(path))
	}
	return nil
}

func unlessMissing(err error) error {
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	return err
}
