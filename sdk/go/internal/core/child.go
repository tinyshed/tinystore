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
		args := []string{"serve", "--dir", absolute, "--stdio"}
		if clock != "" {
			args = append(args, "--clock", clock)
		}
		args = append(args, started.storeFlags()...)
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
			return nil, Wrap(Closed, err, "cannot start the private server %s", path)
		}
		tail := &lines{}
		drained := make(chan struct{})
		go func() {
			// a pipe nobody drains stops the server once it fills
			tail.drain(stderr)
			close(drained)
		}()
		var writing sync.Mutex
		session := NewSession(func(b []byte) error {
			writing.Lock()
			defer writing.Unlock()
			_, err := stdin.Write(b)
			return err
		}, Hello{Client: Client})
		exited := make(chan struct{})
		go func() {
			Read(stdout, session, func(error) error {
				<-drained
				_ = cmd.Wait()
				close(exited)
				return Errorf(Closed, "the private server exited with %d: %s", cmd.ProcessState.ExitCode(), tail.last(2000))
			})
		}()
		closing := func() {
			// a child told to leave by the end of its stdin drains and exits; one that does not is killed
			_ = stdin.Close()
			select {
			case <-exited:
			case <-time.After(10 * time.Second):
				_ = cmd.Process.Kill()
				<-exited
			}
		}
		c := NewConnection(session, stdinTransport{stdin, &writing}, closing)
		if err := c.Handshake(ctx); err != nil {
			_ = cmd.Process.Kill()
			return nil, Wrap(Closed, err, "the private server did not start: %s", tail.last(2000))
		}
		return c, nil
	}
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
