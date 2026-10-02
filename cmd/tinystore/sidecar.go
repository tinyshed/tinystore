package main

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/tinyshed/tinystore/server/reach"
)

const (
	sidecarWait   = 5 * time.Second // how long a sidecar the tool started has to answer
	sidecarStarts = 2               // starts before the tool gives up, the second after a lost race
)

// clientName is how the tool introduces itself, which a server's log shows
func clientName() string {
	return "tinystore-cli/" + version()
}

// reachStore connects to the server serving dir, and starts the directory's
// sidecar when none answers, as an SDK does: a detached serve --local, which
// leaves once it is idle. A serve that exits held lost a race to another,
// whose SERVE is then waited for.
func reachStore(ctx context.Context, dir string) (*reach.Conn, error) {
	absolute, err := filepath.Abs(dir)
	if err != nil {
		return nil, err
	}
	conn, err := reach.Found(ctx, absolute, clientName())
	if err == nil {
		return conn, nil
	}
	if _, err = os.Stat(filepath.Join(absolute, "LOCK")); err != nil {
		return nil, fmt.Errorf("%s holds no store: a store's directory has its LOCK, and one is made by its "+
			"application or by tinystore serve, never by a read", dir)
	}
	self, err := os.Executable()
	if err != nil {
		return nil, err
	}
	log := filepath.Join(absolute, "server", "serve.log")
	why := "no sidecar answered"
	for range sidecarStarts {
		conn, held, err := startSidecar(ctx, self, absolute, log)
		if conn != nil || err != nil {
			return conn, err
		}
		if held {
			why = "another process holds the directory, and no sidecar of it answers"
		}
	}
	return nil, fmt.Errorf("%s: %s", why, tail(log))
}

// startSidecar starts a sidecar of dir and waits for a server to answer
// through SERVE, its own or the one another start won with. held says the
// one it started found the directory held, and none answered meanwhile.
func startSidecar(ctx context.Context, self, dir, log string) (conn *reach.Conn, held bool, err error) {
	//nolint:gosec // this executable, serving the directory a person named
	child := exec.CommandContext(context.WithoutCancel(ctx), self, "serve", "--dir", dir, "--local", "--log", log)
	detach(child)
	if err = child.Start(); err != nil {
		return nil, false, err
	}
	exited := make(chan int, 1)
	go func() {
		_ = child.Wait() //nolint:errcheck // the exit code says what the caller needs
		exited <- child.ProcessState.ExitCode()
	}()

	deadline := time.Now().Add(sidecarWait)
	for pause := 5 * time.Millisecond; time.Now().Before(deadline); pause = min(2*pause, 100*time.Millisecond) {
		if conn, err = reach.Found(ctx, dir, clientName()); err == nil {
			return conn, false, nil
		}
		select {
		case code := <-exited:
			if code != exitHeld {
				return nil, false, fmt.Errorf("the sidecar exited with %d: %s", code, tail(log))
			}
			held, exited = true, nil
		case <-time.After(pause):
		case <-ctx.Done():
			return nil, false, ctx.Err()
		}
	}
	return nil, held, nil
}

// tail is the last lines of a sidecar's log, which say why it ended
func tail(log string) string {
	text, err := os.ReadFile(log) //nolint:gosec // the log the tool's own sidecar writes
	if errors.Is(err, os.ErrNotExist) || len(text) == 0 {
		return "it wrote no log"
	}
	lines := strings.Split(strings.TrimSpace(string(text)), "\n")
	return strings.Join(lines[max(len(lines)-5, 0):], "\n")
}
