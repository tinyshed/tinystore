package server

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/server/internal/client"
	"github.com/tinyshed/tinystore/server/internal/pipe"
	"github.com/tinyshed/tinystore/server/internal/private"
	"github.com/tinyshed/tinystore/server/wire"
)

// SERVE is written whole, only by the server whose store holds the directory's
// lock, in a directory its owner alone may enter. It names the endpoint a
// client finds the server by and the secret it proves itself with.
func TestServeIsWrittenWholeUnderTheLock(t *testing.T) {
	ts := startTestServer(t, Options{Version: "0.4.0"})
	l, unpublish, err := ts.server.Publish(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	go func() {
		if served := ts.server.Serve(context.Background(), l); served != nil {
			t.Errorf("serve: %v", served)
		}
	}()
	dir := filepath.Join(ts.root, "server")
	if err = private.Check(dir); err != nil {
		t.Fatal(err)
	}
	want := []string{"SERVE"}
	if runtime.GOOS != "windows" {
		want = append(want, "tinystore.sock")
	}
	if names := entriesOf(t, dir); !slices.Equal(names, want) {
		t.Fatalf("server/ holds %v, not %v", names, want)
	}

	published := readServe(t, dir)
	instance := base64.RawURLEncoding.EncodeToString(ts.server.Instance())
	secret := base64.RawURLEncoding.EncodeToString(ts.server.secret)
	if published.Protocol != wire.Protocol || published.Server != "0.4.0" || published.PID != os.Getpid() ||
		published.Instance != instance || published.Secret != secret ||
		!slices.Equal(published.Endpoints, []string{l.Addr()}) {
		t.Fatalf("SERVE says %+v", published)
	}
	conn, err := client.Found(t.Context(), ts.root, wire.Hello{})
	if err != nil {
		t.Fatal(err)
	}
	if string(conn.Welcome.Instance) != string(ts.server.Instance()) || conn.Welcome.Capability != wire.Admin {
		t.Fatalf("the server found through SERVE welcomes as %+v", conn.Welcome)
	}
	if err = conn.Close(); err != nil {
		t.Fatal(err)
	}

	if _, _, err = ts.server.Publish(t.Context()); !errors.Is(err, tinystore.ErrInUse) {
		t.Errorf("a second publish of one store: %v", err)
	}
	if _, err = tinystore.Open(t.Context(), ts.root, tinystore.Options{}); !errors.Is(err, tinystore.ErrInUse) {
		t.Errorf("a second store on the published directory: %v", err)
	}
	if err = unpublish(); err != nil {
		t.Fatal(err)
	}
	if names := entriesOf(t, dir); len(names) != 0 {
		t.Fatalf("server/ holds %v after its server unpublished", names)
	}
}

// a local HELLO's challenge is answered with the proof SERVE's secret makes;
// a remote one, whose server TLS proves, and one without a challenge get none
func TestAServerProvesItselfOnlyToALocalChallenge(t *testing.T) {
	token := newToken(t)
	tokens, err := ParseTokens([]byte("admin " + token))
	if err != nil {
		t.Fatal(err)
	}
	ts := startTestServer(t, Options{Tokens: tokens})
	challenge := slices.Repeat([]byte{7}, wire.ChallengeSize)
	published := wire.Published{Secret: base64.RawURLEncoding.EncodeToString(ts.server.secret)}

	if local := ts.dial(t, wire.Hello{Challenge: challenge}); !published.Proves(challenge, local.Welcome.Proof) {
		t.Errorf("a local challenge answered with %x", local.Welcome.Proof)
	}
	if remote := ts.dialAt(t, ts.remote, wire.Hello{Token: token, Challenge: challenge}); remote.Welcome.Proof != nil {
		t.Errorf("a remote challenge answered with %x", remote.Welcome.Proof)
	}
	if plain := ts.dial(t, wire.Hello{}); plain.Welcome.Proof != nil {
		t.Errorf("a HELLO without a challenge answered with %x", plain.Welcome.Proof)
	}
	if goAway := ts.hello(t, ts.endpoint, wire.Hello{Challenge: challenge[1:]}); goAway.Code != wire.CodeProtocol {
		t.Errorf("a challenge of %d bytes: %+v", wire.ChallengeSize-1, goAway)
	}
}

// a process that took the endpoint of a server gone cannot prove it read the
// SERVE that server left, so a client finds it out before its first call
func TestAnEndpointTakenAfterItsServerLeftCannotProveItself(t *testing.T) {
	gone := startTestServer(t, Options{})
	l, unpublish, err := gone.server.Publish(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	dir := filepath.Join(gone.root, "server")
	left, err := os.ReadFile(filepath.Join(dir, "SERVE"))
	if err != nil {
		t.Fatal(err)
	}
	if err = errors.Join(l.Close(), unpublish()); err != nil {
		t.Fatal(err)
	}
	if err = os.WriteFile(filepath.Join(dir, "SERVE"), left, 0o600); err != nil {
		t.Fatal(err)
	}

	taker := startTestServer(t, Options{})
	taken, err := Listen(t.Context(), readServe(t, dir).Endpoints[0], nil)
	if err != nil {
		t.Fatal(err)
	}
	go func() {
		if served := taker.server.Serve(context.Background(), taken); served != nil {
			t.Errorf("serve: %v", served)
		}
	}()
	if conn, err := client.Found(t.Context(), gone.root, wire.Hello{}); !errors.Is(err, client.ErrNotTheServer) {
		t.Fatalf("a client reached the endpoint's taker: %v", errors.Join(err, closeIfAny(conn)))
	}
}

func closeIfAny(conn *client.Conn) error {
	if conn == nil {
		return nil
	}
	return conn.Close()
}

func entriesOf(t *testing.T, dir string) []string {
	t.Helper()
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for _, entry := range entries {
		names = append(names, entry.Name())
	}
	return names
}

func readServe(t *testing.T, dir string) wire.Published {
	t.Helper()
	text, err := os.ReadFile(filepath.Join(dir, "SERVE"))
	if err != nil {
		t.Fatal(err)
	}
	var published wire.Published
	if err = json.Unmarshal(text, &published); err != nil {
		t.Fatal(err)
	}
	return published
}

// a SERVE a server before it left is gone before the next listens, so that a
// server that cannot listen names no endpoint another process may have taken
func TestAServeLeftBehindGoesBeforeTheServerListens(t *testing.T) {
	ts := startTestServer(t, Options{})
	dir := filepath.Join(ts.root, "server")
	takeTheEndpoint(t, dir)
	if err := os.WriteFile(filepath.Join(dir, "SERVE"), []byte(`{"protocol":1}`), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, _, err := ts.server.Publish(t.Context()); err == nil {
		t.Fatal("published at an endpoint another holds")
	}
	if _, err := os.Stat(filepath.Join(dir, "SERVE")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("the SERVE left behind, once its server could not listen: %v", err)
	}
}

// takeTheEndpoint makes the endpoint of dir's sidecar another's: a pipe of its
// name on Windows, elsewhere a directory where its socket belongs
func takeTheEndpoint(t *testing.T, dir string) {
	t.Helper()
	endpoint, err := localEndpoint(dir)
	if err != nil {
		t.Fatal(err)
	}
	name, isPipe := strings.CutPrefix(endpoint, "pipe:")
	if !isPipe {
		if err = os.MkdirAll(filepath.Join(strings.TrimPrefix(endpoint, "unix://"), "taken"), 0o700); err != nil {
			t.Fatal(err)
		}
		return
	}
	var taken *pipe.Listener
	if taken, err = pipe.Listen(name); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = taken.Close() })
	if err = os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
}

// Windows refuses to rename over or remove a file another handle holds without
// sharing its deletion, as Go and Python open files. A change to SERVE that a
// client reading it holds up is tried again, and no other.
func TestAChangeHeldUpByAReaderIsTriedAgain(t *testing.T) {
	tries := 0
	held := func() error {
		if tries++; tries < 3 {
			return &os.PathError{Op: "remove", Path: "SERVE", Err: errorSharingViolation}
		}
		return nil
	}
	err := whileHeld(held)
	if runtime.GOOS == "windows" && (err != nil || tries != 3) {
		t.Fatalf("a change held up twice: %d tries, %v", tries, err)
	}
	if runtime.GOOS != "windows" && (err == nil || tries != 1) {
		t.Fatalf("a change refused off Windows: %d tries, %v", tries, err)
	}

	tries = 0
	missing := func() error { tries++; return os.ErrNotExist }
	if err = whileHeld(missing); !errors.Is(err, os.ErrNotExist) || tries != 1 {
		t.Fatalf("a change of a SERVE that is not there: %d tries, %v", tries, err)
	}
}

// a SERVE a reader holds past every try is an error, and gone once let go
func TestAServeHeldPastEveryTryIsAnError(t *testing.T) {
	if runtime.GOOS != "windows" {
		t.Skip("elsewhere a reader holds up neither a rename nor a remove")
	}
	dir := t.TempDir()
	path := filepath.Join(dir, "SERVE")
	if err := os.WriteFile(path, []byte("{}"), 0o600); err != nil {
		t.Fatal(err)
	}
	reader, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	if err = removeServe(dir); !errors.Is(err, errorSharingViolation) {
		t.Fatalf("a SERVE held past every try: %v", err)
	}
	if err = reader.Close(); err != nil {
		t.Fatal(err)
	}
	if err = removeServe(dir); err != nil {
		t.Fatal(err)
	}
	if _, err = os.Stat(path); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("SERVE once its reader let go: %v", err)
	}
}

// a socket's path that does not fit a sockaddr_un moves to a directory of the
// user's own, named after the store
func TestALongSocketPathMovesToTheUsersOwnDirectory(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("Windows serves a named pipe, which has no such path")
	}
	// a runtime directory as short as a user's, which t.TempDir is not on macOS
	runtimeDir, err := os.MkdirTemp("", "xdg")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(runtimeDir) })
	t.Setenv("XDG_RUNTIME_DIR", runtimeDir)
	long := filepath.Join(t.TempDir(), string(slices.Repeat([]byte("d"), 120)), "server")
	endpoint, err := localEndpoint(long)
	if err != nil {
		t.Fatal(err)
	}
	socket := endpoint[len("unix://"):]
	if len(socket) > socketPathMost() || filepath.Dir(filepath.Dir(socket)) != os.Getenv("XDG_RUNTIME_DIR") {
		t.Fatalf("a long store's socket at %s", socket)
	}
	if err = private.Check(filepath.Dir(socket)); err != nil {
		t.Fatal(err)
	}
}

// a server waits for its last connection to end, and then for idle, before it
// says it is idle
func TestAServerGoesIdleAfterItsLastConnection(t *testing.T) {
	idle := 200 * time.Millisecond
	start := time.Now() // before the server, whose quiet begins when it is made
	ts := startTestServer(t, Options{})
	if err := ts.server.WaitIdle(t.Context(), idle); err != nil || time.Since(start) < idle {
		t.Fatalf("a server without connections went idle after %s: %v", time.Since(start), err)
	}

	conn := ts.dial(t, wire.Hello{})
	idled := make(chan error, 1)
	go func() { idled <- ts.server.WaitIdle(t.Context(), idle) }()
	select {
	case <-idled:
		t.Fatal("idle with a connection open")
	case <-time.After(3 * idle):
	}
	closed := time.Now()
	if err := conn.Close(); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-idled:
		if err != nil || time.Since(closed) < idle {
			t.Fatalf("idle %s after the last connection: %v", time.Since(closed), err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("still not idle after its last connection ended")
	}
}
