package main

import (
	"context"
	"crypto/tls"
	"errors"
	"flag"
	"fmt"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"runtime/debug"
	"strings"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/records/console"
	"github.com/tinyshed/tinystore/server"
)

const serveUsage = `usage:
  tinystore serve <dir>                               the directory's sidecar until Ctrl+C, found through SERVE
  tinystore serve --dir <dir> --stdio [--clock <time>]  a private child: frames on stdin and stdout; --clock runs
                                                      it on a test's clock, at <time> (RFC 3339) until moved
  tinystore serve --dir <dir> --local [--idle 30s]    the directory's shared sidecar, published in <dir>/server/SERVE
  tinystore serve --dir <dir> --listen tls://<host:port> --tls-cert <file> --tls-key <file> --tokens <file>
flags:
  --memory <bytes>   the most every engine's work holds at once; 1 GiB with --listen, 0 for no bound
  --log <file>       append the log to file, owner-only, rather than to stderr, the error serve ends with included`

// errHeld is a directory another store holds: the sidecar a client asked for
// is running already, and SERVE says where
var errHeld = errors.New("the directory is held by another store")

// the exit code of a serve that found the directory held, which a client
// starting a sidecar reads as another having won
const exitHeld = 3

// remoteMemory is a remote server's store memory when --memory does not say.
//
// Without a bound a data client's statement could make SQLite allocate a
// gigabyte. A local client is the same user's own, so it gets no default.
const remoteMemory = 1 << 30

// processStreams are the streams serve runs on: a private child's frames are on stdin
// and stdout, and every server's logs on stderr
type processStreams struct {
	stdin          io.Reader
	stdout, stderr io.WriteCloser
}

type serveFlags struct {
	dir, listen, tlsCert, tlsKey, tokens, log string
	stdio, local                              bool
	foreground                                bool // serve <dir>: a person's, until Ctrl+C, rather than a client's
	idle                                      time.Duration
	memory                                    int64
	clock                                     string    // --clock as given
	clockAt                                   time.Time // where a test's clock starts, when --clock gives one
}

func parseServe(args []string, stderr io.Writer) (serveFlags, error) {
	var asked serveFlags
	flags := flag.NewFlagSet("serve", flag.ContinueOnError)
	flags.SetOutput(stderr)
	flags.Usage = func() { fmt.Fprintln(stderr, serveUsage) }
	flags.StringVar(&asked.dir, "dir", "", "")
	flags.BoolVar(&asked.stdio, "stdio", false, "")
	flags.BoolVar(&asked.local, "local", false, "")
	flags.DurationVar(&asked.idle, "idle", 30*time.Second, "")
	flags.StringVar(&asked.listen, "listen", "", "")
	flags.StringVar(&asked.tlsCert, "tls-cert", "", "")
	flags.StringVar(&asked.tlsKey, "tls-key", "", "")
	flags.StringVar(&asked.tokens, "tokens", "", "")
	flags.Int64Var(&asked.memory, "memory", 0, "")
	flags.StringVar(&asked.log, "log", "", "")
	flags.StringVar(&asked.clock, "clock", "", "")
	dir, err := parseWithDir(flags, args, &asked.dir)
	if err != nil {
		return serveFlags{}, err
	}
	if dir == "." && asked.dir == "" {
		return serveFlags{}, errors.New(serveUsage) // a store made in whatever directory a person stood in
	}
	asked.dir = dir
	if asked.listen != "" && !given(flags, "memory") {
		asked.memory = remoteMemory
	}
	if !asked.stdio && !asked.local && asked.listen == "" {
		asked.local, asked.foreground = true, true
		if !given(flags, "idle") {
			asked.idle = 0
		}
	}
	overTLS := strings.HasPrefix(asked.listen, "tls://")
	switch {
	case asked.stdio == (asked.local || asked.listen != ""):
		return serveFlags{}, fmt.Errorf("serve --stdio alone, or --local, --listen or both\n%s", serveUsage)
	case asked.listen != "" && asked.tokens == "":
		return serveFlags{}, errors.New("serve --listen needs --tokens: a remote connection without a token is refused")
	case overTLS != (asked.tlsCert != "") || overTLS != (asked.tlsKey != ""):
		return serveFlags{}, errors.New("serve --listen tls:// takes --tls-cert and --tls-key, and nothing else does")
	case asked.idle < 0:
		return serveFlags{}, errors.New("serve --idle is a duration, 0 for never")
	case asked.clock != "" && !asked.stdio:
		return serveFlags{}, errors.New("serve --clock is a private server's, with --stdio: a shared store runs on " +
			"the system's time")
	}
	if asked.clock != "" {
		if asked.clockAt, err = time.Parse(time.RFC3339Nano, asked.clock); err != nil {
			return serveFlags{}, fmt.Errorf("serve --clock is a time such as 2026-10-03T09:00:00Z: %w", err)
		}
	}
	return asked, nil
}

// given says whether the command line set a flag, rather than leaving its default
func given(flags *flag.FlagSet, name string) bool {
	set := false
	flags.Visit(func(f *flag.Flag) { set = set || f.Name == name })
	return set
}

// serving is what serve was asked and what the files it names hold, read
// before the store opens, so that a file serve cannot read opens nothing
type serving struct {
	serveFlags
	options server.Options
	tls     *tls.Config // a tls:// listener's certificate
	stderr  io.Writer   // where a foreground serve tells its person where apps find it
}

func readServing(asked serveFlags) (serving, error) {
	read := serving{serveFlags: asked, options: server.Options{
		Version: version(),
		Sidecar: asked.local && !asked.foreground, // a client's, which a client of a newer release replaces
	}}
	if asked.tlsCert != "" {
		certificate, err := tls.LoadX509KeyPair(asked.tlsCert, asked.tlsKey)
		if err != nil {
			return serving{}, err
		}
		read.tls = &tls.Config{Certificates: []tls.Certificate{certificate}, MinVersion: tls.VersionTLS12}
	}
	if asked.tokens != "" {
		text, err := os.ReadFile(asked.tokens)
		if err != nil {
			return serving{}, err
		}
		if read.options.Tokens, err = server.ParseTokens(text); err != nil {
			return serving{}, err
		}
	}
	return read, nil
}

// serve runs a server of the store in a directory until its private connection
// ends, it has been idle, or ctx ends.
//
// Ending closes it: the streams running finish, SERVE goes, and the store
// lets the directory go.
func serve(ctx context.Context, args []string, streams processStreams) error {
	asked, err := parseServe(args, streams.stderr)
	if err != nil {
		return err
	}
	logs, closeLogs, err := openLog(asked.log, streams.stderr)
	if err != nil {
		return err
	}
	logger := slog.New(slog.NewTextHandler(logs, nil))
	if file, ok := logs.(*os.File); ok && asked.foreground && isTerminal(file) {
		logger = slog.New(console.Handler("serve", console.Pretty))
	}
	err = serveLogged(ctx, asked, streams, logger)
	if err != nil && asked.log != "" {
		logger.Error("serve ended", "err", err)
	}
	return errors.Join(err, closeLogs())
}

// openLog is where serve's log goes: stderr, or the file --log names, made
// owner-only in directories made so, since a log names keys and paths.
//
// A sidecar a client starts in the background has no stderr anyone reads, so
// the file is opened first and keeps every line, the one saying why serve
// ended included. Nothing rotates it.
func openLog(path string, stderr io.Writer) (io.Writer, func() error, error) {
	if path == "" {
		return stderr, func() error { return nil }, nil
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return nil, nil, fmt.Errorf("serve --log: %w", err)
	}
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_APPEND, 0o600) //nolint:gosec // the file asked for
	if err != nil {
		return nil, nil, fmt.Errorf("serve --log: %w", err)
	}
	return file, file.Close, nil
}

func serveLogged(ctx context.Context, asked serveFlags, streams processStreams, logger *slog.Logger) error {
	read, err := readServing(asked)
	if err != nil {
		return err
	}
	if os.Getenv("GOGC") == "" {
		debug.SetGCPercent(400)
	}
	opening := tinystore.Options{Logger: logger, Memory: asked.memory}
	if asked.clock != "" {
		read.options.Clock = server.NewClock(asked.clockAt)
		opening.Clock = read.options.Clock.Now
	}
	store, err := tinystore.Open(ctx, asked.dir, opening)
	if errors.Is(err, tinystore.ErrInUse) {
		return fmt.Errorf("%w: %w", errHeld, err)
	}
	if err != nil {
		return err
	}
	ctx, stop := context.WithCancel(ctx) // an admin's server.stop ends it too
	defer stop()
	read.options.Stop = stop
	closing := context.WithoutCancel(ctx) // the end of ctx is what usually closes
	srv, err := server.New(store, read.options)
	if err != nil {
		return errors.Join(err, store.Close(closing))
	}
	if asked.stdio {
		err = servePrivate(ctx, srv, streams)
	} else {
		read.stderr = streams.stderr
		err = serveShared(ctx, srv, read, logger)
	}
	return errors.Join(err, closeServer(closing, srv), store.Close(closing))
}

// the time closing gives the streams running before it cancels them
const closeWait = 10 * time.Second

func closeServer(ctx context.Context, srv *server.Server) error {
	ctx, cancel := context.WithTimeout(ctx, closeWait)
	defer cancel()
	return srv.Close(ctx)
}

// servePrivate serves the parent's connection on stdin and stdout until the
// parent ends its side, or ctx ends
func servePrivate(ctx context.Context, srv *server.Server, streams processStreams) error {
	served := make(chan error, 1)
	go func() {
		served <- srv.ServeConn(context.WithoutCancel(ctx), newStdio(streams.stdin, streams.stdout), false)
	}()
	select {
	case err := <-served:
		return err
	case <-ctx.Done():
		return nil
	}
}

// stdio is a private child's connection: what its parent writes and what it
// reads.
//
// stdin is read on a goroutine of its own and never closed, because closing a
// blocking stdin ends no read waiting on it and a console's close waits for
// that read. Closing the connection lets the read go, and the process's exit
// ends the goroutine.
type stdio struct {
	in  *io.PipeReader
	out io.WriteCloser
}

func newStdio(stdin io.Reader, stdout io.WriteCloser) stdio {
	in, fromParent := io.Pipe()
	go func() {
		_, err := io.Copy(fromParent, stdin)
		_ = fromParent.CloseWithError(err) // nil, the parent's end, reads as io.EOF
	}()
	return stdio{in: in, out: stdout}
}

func (s stdio) Read(p []byte) (int, error)  { return s.in.Read(p) }
func (s stdio) Write(p []byte) (int, error) { return s.out.Write(p) }
func (s stdio) CloseWrite() error           { return s.out.Close() }
func (s stdio) Close() error                { return errors.Join(s.out.Close(), s.in.Close()) }

// serveShared listens where it was asked, publishing the local endpoint in
// SERVE, and serves until it has been idle, a listener fails or ctx ends
func serveShared(ctx context.Context, srv *server.Server, read serving, logger *slog.Logger) (err error) {
	var listeners []server.Listener
	if read.local {
		l, unpublish, publishErr := srv.Publish(ctx)
		if publishErr != nil {
			return publishErr
		}
		defer func() { err = errors.Join(err, unpublish()) }()
		listeners = append(listeners, l)
	}
	if read.listen != "" {
		l, listenErr := listenRemote(ctx, read)
		if listenErr != nil {
			return errors.Join(listenErr, closeAll(listeners))
		}
		listeners = append(listeners, l)
	}

	failed := make(chan error, len(listeners))
	var endpoints []string
	for _, l := range listeners {
		endpoints = append(endpoints, l.Addr())
		go func() { failed <- srv.Serve(context.WithoutCancel(ctx), l) }()
	}
	if read.foreground {
		showServing(read.stderr, read.dir, endpoints)
	} else {
		logger.Info("serving", "endpoints", strings.Join(endpoints, " "), "pid", os.Getpid())
	}
	return waitShared(ctx, srv, read.serveFlags, failed)
}

// showServing tells the person who started serve where apps find it:
//
//	● serving ./data · Ctrl+C to stop
//	  apps find it through   ./data/server/SERVE
//	  endpoint               pipe:tinystore-015ef87b270a1914
func showServing(stderr io.Writer, dir string, endpoints []string) {
	p := painterFor(stderr)
	text := fmt.Sprintf("%s serving %s %s\n  %-22s %s\n", p.in(green, "●"), p.in(bold, dir),
		p.in(dim, "· Ctrl+C to stop"), "apps find it through", filepath.Join(dir, "server", "SERVE"))
	for _, endpoint := range endpoints {
		text += fmt.Sprintf("  %-22s %s\n", "endpoint", endpoint)
	}
	_, _ = io.WriteString(stderr, text+"\n") //nolint:errcheck // a terminal that refuses has nobody to tell
}

func listenRemote(ctx context.Context, read serving) (server.Listener, error) {
	l, err := server.Listen(ctx, read.listen, read.tls)
	if err == nil && !l.Remote() {
		err = errors.Join(fmt.Errorf("serve --listen %s is local; --local serves the directory's own", read.listen),
			l.Close())
	}
	return l, err
}

func closeAll(listeners []server.Listener) error {
	var err error
	for _, l := range listeners {
		err = errors.Join(err, l.Close())
	}
	return err
}

// waitShared waits for what ends a shared server: idleness, when it serves the
// directory's sidecar, a listener that failed, or ctx
func waitShared(ctx context.Context, srv *server.Server, asked serveFlags, failed <-chan error) error {
	idled := make(chan error, 1)
	if asked.local && asked.idle > 0 {
		go func() { idled <- srv.WaitIdle(ctx, asked.idle) }()
	}
	select {
	case <-ctx.Done():
		return nil
	case <-idled: // idle, or ctx's end, which is no failure either
		return nil
	case err := <-failed:
		return err
	}
}

// version is this binary's module version, as WELCOME and SERVE state it: the
// tag a release is built from, as go build stamps it from the checkout, and a
// pseudo-version or (devel) otherwise.
func version() string {
	if info, ok := debug.ReadBuildInfo(); ok && info.Main.Version != "" {
		return info.Main.Version
	}
	return "(devel)"
}
