package core

import (
	"bufio"
	"context"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"
)

// Started is what a server or a core this process starts is started with; one
// found running keeps its own.
type Started struct {
	// Durability is full or os, or "" for the store's own.
	Durability        string
	EncryptionKeyFile string
	// OlderEncryptionKeys are the files of the store's older keys, which
	// open what they sealed.
	OlderEncryptionKeys []string
	// Idle is how long a sidecar stays once its last connection has gone, as
	// tinystore serve --idle takes it.
	Idle string
	// SelfMetrics measures the store into its own metrics, and the machine
	// too unless WithoutSystem.
	SelfMetrics   bool
	WithoutSystem bool
	// MaxSeries is the metrics series the store holds at most, or 0 for the
	// store's own 100,000.
	MaxSeries uint64
}

// storeFlags are the flags of tinystore serve that say how the store opens.
func (s Started) storeFlags() []string {
	var flags []string
	if s.Durability != "" {
		flags = append(flags, "--durability", s.Durability)
	}
	if s.EncryptionKeyFile != "" {
		key, _ := filepath.Abs(s.EncryptionKeyFile)
		flags = append(flags, "--encryption-key-file", key)
	}
	for _, older := range s.OlderEncryptionKeys {
		key, _ := filepath.Abs(older)
		flags = append(flags, "--older-encryption-key", key)
	}
	if s.SelfMetrics {
		flags = append(flags, "--self-metrics")
	}
	if s.SelfMetrics && s.WithoutSystem {
		flags = append(flags, "--no-system-metrics")
	}
	if s.MaxSeries > 0 {
		flags = append(flags, "--max-series", strconv.FormatUint(s.MaxSeries, 10))
	}
	return flags
}

// FindBinary is the tinystore binary: the one given, TINYSTORE_BIN, PATH's,
// or the release's own, fetched once.
func FindBinary(ctx context.Context, given string) (string, error) {
	if given != "" {
		return given, nil
	}
	if found := os.Getenv("TINYSTORE_BIN"); found != "" {
		return found, nil
	}
	if found, err := exec.LookPath("tinystore"); err == nil {
		return found, nil
	}
	found, err := fetchedBinary(ctx)
	if err != nil {
		return "", Wrap(Closed, err, "no tinystore binary: put tinystore on PATH, or set TINYSTORE_BIN")
	}
	return found, nil
}

// ReleaseBinary is TINYSTORE_BIN, or this release's own binary, fetched
// once; never PATH's, which may be the command that asks for it.
func ReleaseBinary(ctx context.Context) (string, error) {
	if found := os.Getenv("TINYSTORE_BIN"); found != "" {
		return found, nil
	}
	return fetchedBinary(ctx)
}

// PrivateChild dials a private child: tinystore serve --stdio, living and
// dying with this process. A test's clock starts where the first child's did,
// and a child started again starts it there.
func PrivateChild(directory string, binary func(context.Context) (string, error), clock string, started Started) func(context.Context) (*Connection, error) {
	absolute, _ := filepath.Abs(directory)
	return func(ctx context.Context) (*Connection, error) {
		path, err := binary(ctx)
		if err != nil {
			return nil, err
		}
		Waiting(ctx, "the private server starts")
		args := []string{"serve", "--dir", absolute, "--stdio"}
		if clock != "" {
			args = append(args, "--clock", clock)
		}
		child, err := startPrivate(path, append(args, started.storeFlags()...))
		if err != nil {
			return nil, err
		}
		c := child.connection()
		if err := c.Handshake(ctx); err != nil {
			return nil, child.failed(err)
		}
		return c, nil
	}
}

// private is a private child that has started, its stderr read to its end:
// a pipe nobody drains stops the server once it fills.
type private struct {
	cmd     *exec.Cmd
	stdin   io.WriteCloser
	stdout  io.Reader
	tail    *lines
	drained chan struct{}
}

func startPrivate(path string, args []string) (*private, error) {
	cmd := exec.Command(path, args...)
	hideWindow(cmd)
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, err
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, err
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return nil, err
	}
	if err := cmd.Start(); err != nil {
		return nil, notStarted("the private server", path, err)
	}
	p := &private{cmd: cmd, stdin: stdin, stdout: stdout, tail: &lines{}, drained: make(chan struct{})}
	go func() {
		p.tail.drain(stderr)
		close(p.drained)
	}()
	return p, nil
}

// connection is the session over the child's stdin and stdout. Its close
// ends stdin, which tells the child to leave, and waits for its exit, since
// until then it holds the directory.
func (p *private) connection() *Connection {
	transport := stdinTransport{p.stdin, &sync.Mutex{}}
	session := NewSession(transport.Write, Hello{Client: Client})
	exited := make(chan struct{})
	go func() {
		Read(p.stdout, session, func(error) error {
			<-p.drained
			_ = p.cmd.Wait()
			close(exited)
			return Errorf(Closed, "the private server exited with %d: %s", p.cmd.ProcessState.ExitCode(), p.tail.last(2000))
		})
	}()
	closing := func() {
		// a child told to leave by the end of its stdin drains and exits; one that does not is killed
		_ = p.stdin.Close()
		select {
		case <-exited:
		case <-time.After(10 * time.Second):
			_ = p.cmd.Process.Kill()
			<-exited
		}
	}
	return NewConnection(session, transport, closing)
}

// failed kills a child whose handshake failed, and says why with what it
// wrote to stderr once: the error of a child that exited carries it already.
func (p *private) failed(err error) error {
	_ = p.cmd.Process.Kill()
	if said := p.tail.last(2000); said != "" && !strings.Contains(err.Error(), said) {
		return Wrap(Closed, err, "the private server did not start: %v: %s", err, said)
	}
	return Wrap(Closed, err, "the private server did not start")
}

type stdinTransport struct {
	stdin   io.WriteCloser
	writing *sync.Mutex
}

func (t stdinTransport) Write(b []byte) error {
	t.writing.Lock()
	defer t.writing.Unlock()
	_, err := t.stdin.Write(b)
	return err
}

func (t stdinTransport) Close() error { return t.stdin.Close() }

// lines keeps the last lines a child wrote to its stderr.
type lines struct {
	mu   sync.Mutex
	kept []string
}

func (l *lines) drain(r io.Reader) {
	scanner := bufio.NewScanner(r)
	scanner.Buffer(make([]byte, 64<<10), 1<<20)
	for scanner.Scan() {
		l.mu.Lock()
		l.kept = append(l.kept, scanner.Text())
		if len(l.kept) > 50 {
			l.kept = l.kept[len(l.kept)-50:]
		}
		l.mu.Unlock()
	}
}

func (l *lines) last(most int) string {
	l.mu.Lock()
	defer l.mu.Unlock()
	text := strings.TrimSpace(strings.Join(l.kept, "\n"))
	if len(text) > most {
		text = text[len(text)-most:]
	}
	return text
}

// lastLines are the last n lines kept that say something.
func (l *lines) lastLines(n int) string {
	l.mu.Lock()
	defer l.mu.Unlock()
	return lastOf(l.kept, n)
}

// lastOf is the last n of lines, blank ones at the end left out.
func lastOf(lines []string, n int) string {
	end := len(lines)
	for end > 0 && strings.TrimSpace(lines[end-1]) == "" {
		end--
	}
	return strings.Join(lines[max(0, end-n):end], "\n")
}
