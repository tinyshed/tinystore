package core

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
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
func Sidecar(directory string, binary func() (string, error), started Started) func(context.Context) (*Connection, error) {
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
		path, err := binary()
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
		cmd := exec.Command(path, args...)
		detach(cmd)
		if err := cmd.Start(); err != nil {
			return nil, Wrap(Closed, err, "cannot start the sidecar %s", path)
		}
		exited := make(chan int, 1)
		go func() {
			_ = cmd.Wait()
			exited <- cmd.ProcessState.ExitCode()
		}()
		var again time.Time
		wait := 5 * time.Millisecond
		code := -1
		for time.Now().Before(deadline) {
			if found := reachServe(ctx, serve); found != nil {
				if found.instance != stopped {
					return found.connection, nil
				}
				found.connection.Close()
			}
			select {
			case code = <-exited:
			default:
			}
			if code >= 0 && code != 3 {
				return nil, Errorf(Closed, "the sidecar exited with %d: %s", code, tailOf(log))
			}
			if code == 3 {
				held = true
				if again.IsZero() {
					again = time.Now().Add(pause)
				}
				if !time.Now().Before(again) {
					break
				}
			}
			select {
			case <-ctx.Done():
				return nil, ctx.Err()
			case <-time.After(wait):
			}
			wait = min(wait*2, 100*time.Millisecond)
		}
		if !time.Now().Before(deadline) {
			why := "no sidecar answered"
			if held {
				why = "another process holds the directory but no sidecar answers"
			}
			return nil, Errorf(Closed, "%s in %s: %s", why, StartTime, tailOf(log))
		}
		pause = min(pause*2, time.Second)
	}
}

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

func tailOf(log string) string {
	text, err := os.ReadFile(log)
	if err != nil {
		return "it wrote no log"
	}
	lines := strings.Split(strings.TrimSpace(string(text)), "\n")
	if len(lines) > 5 {
		lines = lines[len(lines)-5:]
	}
	return strings.Join(lines, "\n")
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
