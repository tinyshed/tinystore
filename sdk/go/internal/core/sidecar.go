package core

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"time"

	"tinyshed.org/tinystore/internal/wire"
)

const (
	// StartTime is how long a client waits for a sidecar to answer, its own
	// or another's, before it gives up.
	StartTime = 15 * time.Second
	// heldPause is the pause before a start that found the directory held
	// starts again, doubling up to a second.
	heldPause = 100 * time.Millisecond
)

// Sidecar finds the directory's sidecar through SERVE or starts one, as
// docs/wire.md says. A sidecar found proves itself before the first call; one
// started exits with 3 when another holds the directory, whose SERVE the
// client then waits for. A sidecar of an older release than this SDK's is
// stopped and replaced; another server of an older release, a person's or a
// program's own, is told of once.
func Sidecar(directory string, binary func(context.Context) (string, error), started Started) func(context.Context) (*Connection, error) {
	absolute, _ := filepath.Abs(directory)
	serve := filepath.Join(absolute, "server", "SERVE")
	log := filepath.Join(absolute, "server", "serve.log")
	told := false
	return func(ctx context.Context) (*Connection, error) {
		found := reachServe(ctx, serve)
		stopped := ""
		if found != nil {
			agreed := found.connection.Session.Agreed()
			if !OlderRelease(agreed.Server, Version) {
				return found.connection, nil
			}
			if found.sidecar && stoppedSidecar(ctx, found.connection) {
				fmt.Fprintf(os.Stderr, "tinystore: the sidecar serving %s was %s, older than this SDK's %s; it finishes its calls, and this SDK's binary serves the directory from now on\n", directory, agreed.Server, Version)
				stopped = found.instance
			} else {
				if !told {
					fmt.Fprintf(os.Stderr, "tinystore: %s is served by %s, older than this SDK's %s; a person or a program started that server, and it runs until they stop it\n", directory, agreed.Server, Version)
				}
				told = true
				return found.connection, nil
			}
		}
		path, err := binary(ctx)
		if err != nil {
			return nil, err
		}
		args := []string{"serve", "--dir", absolute, "--local", "--log-file", log}
		if started.Idle != "" {
			args = append(args, "--idle", started.Idle)
		}
		args = append(args, started.storeFlags()...)
		return startSidecar(ctx, serve, log, path, args, stopped)
	}
}

// startSidecar starts a sidecar and waits for SERVE to name one that answers,
// its own or another client's that won the start. One that exits with 3
// found the directory held, by a winner about to publish or by a sidecar
// still letting go, and is started again after a pause; any other exit is an
// error. A sidecar told to stop answers for a moment after it agreed, until
// SERVE goes, so the one of the instance stopped is passed over.
func startSidecar(ctx context.Context, serve, log, path string, args []string, stopped string) (*Connection, error) {
	deadline := time.Now().Add(StartTime)
	held := false
	pause := heldPause
	for {
		child, err := spawnSidecar(path, args, log)
		if err != nil {
			return nil, err
		}
		c, code, err := child.awaitServe(ctx, serve, stopped, deadline, pause)
		child.release()
		if c != nil || err != nil {
			return c, err
		}
		held = held || code == 3
		if !time.Now().Before(deadline) {
			why := "no sidecar answered"
			if held {
				why = "another process holds the directory but no sidecar answers"
			}
			return nil, Errorf(Closed, "%s in %s: %s", why, StartTime, child.failure(false))
		}
		pause = min(pause*2, time.Second)
	}
}

// spawned is a sidecar this process started, its stderr read while it
// starts: a start that fails before its log opens says why there alone.
type spawned struct {
	exited  chan int
	stderr  *lines
	drained chan struct{}
	read    *os.File
	log     string
	// logged is serve.log's size before it started: the log keeps every
	// run's lines, so this one's begin here
	logged int64
}

func spawnSidecar(path string, args []string, log string) (*spawned, error) {
	logged := int64(0)
	if info, err := os.Stat(log); err == nil {
		logged = info.Size()
	}
	cmd := exec.Command(path, args...)
	detach(cmd)
	// a file, not a writer, so that exec copies nothing and the child's end is its own
	read, write, err := os.Pipe()
	if err != nil {
		return nil, Wrap(Closed, err, "cannot start the sidecar %s", path)
	}
	cmd.Stderr = write
	err = cmd.Start()
	_ = write.Close()
	if err != nil {
		_ = read.Close()
		return nil, notStarted("the sidecar", path, err)
	}
	s := &spawned{exited: make(chan int, 1), stderr: &lines{}, drained: make(chan struct{}), read: read, log: log, logged: logged}
	go func() {
		s.stderr.drain(read)
		close(s.drained)
	}()
	go func() {
		_ = cmd.Wait()
		s.exited <- cmd.ProcessState.ExitCode()
	}()
	return s, nil
}

// awaitServe waits for SERVE to name a sidecar that answers, until the
// deadline or until this one exits: one that found the directory held, exit
// 3, is given up on after the pause, and any other exit is the error.
func (s *spawned) awaitServe(ctx context.Context, serve, stopped string, deadline time.Time, pause time.Duration) (*Connection, int, error) {
	var again time.Time
	wait := 5 * time.Millisecond
	code := -1
	for time.Now().Before(deadline) {
		if found := reachServe(ctx, serve); found != nil {
			if found.instance != stopped {
				return found.connection, code, nil
			}
			found.connection.Close()
		}
		select {
		case code = <-s.exited:
		default:
		}
		if code >= 0 && code != 3 {
			return nil, code, Errorf(Closed, "the sidecar exited with %d: %s", code, s.failure(true))
		}
		if code == 3 {
			if again.IsZero() {
				again = time.Now().Add(pause)
			}
			if !time.Now().Before(again) {
				return nil, code, nil
			}
		}
		select {
		case <-ctx.Done():
			return nil, code, ctx.Err()
		case <-time.After(wait):
		}
		wait = min(wait*2, 100*time.Millisecond)
	}
	return nil, code, nil
}

// release stops reading its stderr once its start is over: what it writes
// there later goes nowhere.
func (s *spawned) release() { _ = s.read.Close() }

// failure is why it stopped without serving: the last lines it wrote to
// stderr, or else the lines its log gained since it started. Once it has
// exited, its stderr is read to its end first, a moment at most.
func (s *spawned) failure(exited bool) string {
	if exited {
		select {
		case <-s.drained:
		case <-time.After(time.Second):
		}
	}
	if said := s.stderr.lastLines(5); said != "" {
		return said
	}
	text, err := os.ReadFile(s.log)
	if err != nil {
		return "it wrote no log"
	}
	// a log shorter than it was is another file, every line of it new
	if int64(len(text)) >= s.logged {
		text = text[s.logged:]
	}
	gained := strings.TrimSpace(string(text))
	if gained == "" {
		return "it wrote no log"
	}
	return lastOf(strings.Split(gained, "\n"), 5)
}

// notStarted is the error of a process that did not start, its cause in the
// words every SDK uses: not found, permission denied, or the system's own.
func notStarted(what, path string, err error) *Error {
	reason := err.Error()
	var pathError *fs.PathError
	switch {
	case errors.Is(err, exec.ErrNotFound), errors.Is(err, fs.ErrNotExist):
		reason = "not found"
	case errors.Is(err, fs.ErrPermission):
		reason = "permission denied"
	case errors.As(err, &pathError):
		reason = pathError.Err.Error()
	}
	return Wrap(Closed, toldAs{reason, err}, "cannot start %s %s", what, path)
}

// toldAs is a cause said in other words, still the cause errors.Is finds.
type toldAs struct {
	text string
	err  error
}

func (t toldAs) Error() string { return t.text }
func (t toldAs) Unwrap() error { return t.err }

// stoppedSidecar asks a sidecar to stop as tinystore stop does, closing the
// connection once it has answered; false when it refuses, the connection open.
func stoppedSidecar(ctx context.Context, c *Connection) bool {
	if _, err := c.Session.Call(ctx, wire.MethodServerStop, wire.Encode(&wire.Empty{})); err != nil {
		return false
	}
	c.Close()
	return true
}

type served struct {
	connection *Connection
	sidecar    bool
	instance   string
}

// reachServe is the sidecar SERVE names once its proof checks, whether SERVE
// calls it a sidecar, and its instance; nil on any failure.
func reachServe(ctx context.Context, serve string) *served {
	text, err := os.ReadFile(serve)
	if err != nil {
		return nil
	}
	var published struct {
		Endpoints []string `json:"endpoints"`
		Secret    string   `json:"secret"`
		Protocol  int      `json:"protocol"`
		Sidecar   bool     `json:"sidecar"`
		Instance  any      `json:"instance"`
	}
	if json.Unmarshal(text, &published) != nil || len(published.Endpoints) == 0 || published.Protocol != ProtocolVersion {
		return nil
	}
	secret, err := base64.RawURLEncoding.DecodeString(strings.TrimRight(published.Secret, "="))
	if err != nil || len(secret) != 32 {
		return nil
	}
	c, err := Dial(ctx, published.Endpoints[0], "", secret, nil)
	if err != nil {
		return nil
	}
	return &served{connection: c, sidecar: published.Sidecar, instance: fmt.Sprint(published.Instance)}
}

var release = regexp.MustCompile(`^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.]+))?$`)

// OlderRelease says whether a server's release is older than the SDK's own;
// a build that is no release, a development copy's or a pseudo-version, is neither.
func OlderRelease(server, own string) bool {
	theirs, ours := releaseOf(server), releaseOf(own)
	return theirs != nil && ours != nil && compareReleases(theirs, ours) < 0
}

type version struct {
	core [3]int
	pre  []string
}

func releaseOf(text string) *version {
	parts := release.FindStringSubmatch(text)
	if parts == nil {
		return nil
	}
	v := &version{}
	for i := range 3 {
		v.core[i], _ = strconv.Atoi(parts[i+1])
	}
	if v.core == [3]int{} {
		return nil
	}
	if parts[4] != "" {
		v.pre = strings.Split(parts[4], ".")
	}
	return v
}

// compareReleases is semantic versioning's order: a release after its
// pre-releases, rc.2 before rc.10.
func compareReleases(a, b *version) int {
	for i := range 3 {
		if a.core[i] != b.core[i] {
			return a.core[i] - b.core[i]
		}
	}
	if len(a.pre) == 0 || len(b.pre) == 0 {
		return len(b.pre) - len(a.pre)
	}
	for i := range min(len(a.pre), len(b.pre)) {
		x, y := a.pre[i], b.pre[i]
		if x == y {
			continue
		}
		nx, errX := strconv.Atoi(x)
		ny, errY := strconv.Atoi(y)
		if errX == nil && errY == nil {
			return nx - ny
		}
		return strings.Compare(x, y)
	}
	return len(a.pre) - len(b.pre)
}
