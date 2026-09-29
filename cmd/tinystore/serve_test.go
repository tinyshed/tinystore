package main

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"io"
	"io/fs"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/server/wire"
)

// lockedBuffer is a server's stderr, which its goroutines write while the
// test reads
type lockedBuffer struct {
	mu   sync.Mutex
	text bytes.Buffer
}

func (b *lockedBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.text.Write(p)
}

func (b *lockedBuffer) Close() error { return nil }

func (b *lockedBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.text.String()
}

// start runs serve on a goroutine, and hands back what it returned once it has
func start(ctx context.Context, t *testing.T, args ...string) <-chan error {
	t.Helper()
	logs := &lockedBuffer{}
	t.Cleanup(func() {
		if t.Failed() {
			t.Logf("the server's logs:\n%s", logs)
		}
	})
	done := make(chan error, 1)
	go func() {
		done <- serve(ctx, args, console{stdin: strings.NewReader(""), stdout: logs, stderr: logs})
	}()
	return done
}

func ended(t *testing.T, done <-chan error) error {
	t.Helper()
	select {
	case err := <-done:
		return err
	case <-time.After(20 * time.Second):
		t.Fatal("serve did not return")
		return nil
	}
}

// waitServe waits for a SERVE whose instance is not the one given, written by
// the server serving, which is not to return first
func waitServe(t *testing.T, dir, not string, serving <-chan error) wire.Published {
	t.Helper()
	deadline := time.Now().Add(20 * time.Second)
	for time.Now().Before(deadline) {
		text, err := os.ReadFile(filepath.Join(dir, "server", "SERVE"))
		var published wire.Published
		if err == nil && json.Unmarshal(text, &published) == nil && published.Instance != not {
			return published
		}
		select {
		case err = <-serving:
			t.Fatalf("serve returned before it wrote SERVE: %v", err)
		case <-time.After(10 * time.Millisecond):
		}
	}
	t.Fatal("no SERVE was written")
	return wire.Published{}
}

// handshake reaches the endpoint SERVE names as an SDK does, a HELLO carrying
// a challenge, and returns the WELCOME that answers it with the proof SERVE's
// secret makes
func handshake(t *testing.T, published wire.Published) wire.Welcome {
	t.Helper()
	challenge := make([]byte, wire.ChallengeSize)
	if _, err := rand.Read(challenge); err != nil {
		t.Fatal(err)
	}
	conn := dialEndpoint(t, published.Endpoints[0])
	defer conn.Close()
	hello := wire.Hello{Protocol: wire.Protocol, Client: "tinystore-cmd-test", Challenge: challenge}
	if _, err := conn.Write(wire.AppendFrame(nil, wire.Header{Kind: wire.KindHello}, hello.Append(nil))); err != nil {
		t.Fatal(err)
	}
	welcome := readWelcome(t, conn)
	if !published.Proves(challenge, welcome.Proof) {
		t.Fatalf("the server SERVE names answers its challenge with %x", welcome.Proof)
	}
	if base64.RawURLEncoding.EncodeToString(welcome.Instance) != published.Instance {
		t.Fatalf("SERVE names instance %s, and its server welcomes as %x", published.Instance, welcome.Instance)
	}
	return welcome
}

func dialEndpoint(t *testing.T, endpoint string) io.ReadWriteCloser {
	t.Helper()
	scheme, address, _ := strings.Cut(endpoint, ":")
	var conn io.ReadWriteCloser
	var err error
	switch scheme {
	case "unix":
		conn, err = net.Dial("unix", strings.TrimPrefix(address, "//"))
	case "pipe":
		conn, err = os.OpenFile(`\\.\pipe\`+address, os.O_RDWR, 0)
	default:
		t.Fatalf("SERVE names %s", endpoint)
	}
	if err != nil {
		t.Fatal(err)
	}
	return conn
}

func readWelcome(t *testing.T, conn io.Reader) wire.Welcome {
	t.Helper()
	reader := wire.NewReader(conn, 1<<20)
	h, err := reader.Header()
	if err != nil {
		t.Fatal(err)
	}
	body, err := reader.Body(nil)
	if err != nil || h.Kind != wire.KindWelcome {
		t.Fatalf("a %s where the WELCOME belongs: %v", h.Kind, err)
	}
	var welcome wire.Welcome
	if err = welcome.Decode(body); err != nil {
		t.Fatal(err)
	}
	return welcome
}

func mustRelease(t *testing.T, dir string) {
	t.Helper()
	store, err := tinystore.Open(t.Context(), dir, tinystore.Options{})
	if err != nil {
		t.Fatalf("the directory after its server left: %v", err)
	}
	if err = store.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
}

// the directory's sidecar is found through SERVE, answers as the instance SERVE
// names, and leaves once it has been idle, taking SERVE and the lock with it
func TestTheSidecarIsFoundThroughServeAndLeavesWhenIdle(t *testing.T) {
	dir := t.TempDir()
	done := start(t.Context(), t, "--dir", dir, "--local", "--idle", "300ms")
	published := waitServe(t, dir, "", done)
	if welcome := handshake(t, published); welcome.Capability != wire.Admin {
		t.Fatalf("a local connection is %s", welcome.Capability)
	}
	if err := ended(t, done); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(dir, "server", "SERVE")); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("SERVE after its server left: %v", err)
	}
	mustRelease(t, dir)
}

// a second server of a directory finds it held and says so with its code
func TestASecondServeOfADirectoryExitsHeld(t *testing.T) {
	dir := t.TempDir()
	ctx, cancel := context.WithCancel(t.Context())
	first := start(ctx, t, "--dir", dir, "--local", "--idle", "0")
	waitServe(t, dir, "", first)
	if err := ended(t, start(ctx, t, "--dir", dir, "--local")); !errors.Is(err, errHeld) {
		t.Fatalf("a second server of one directory: %v", err)
	}
	cancel()
	if err := ended(t, first); err != nil {
		t.Fatal(err)
	}
	mustRelease(t, dir)
}

// a SERVE left by a server that died does not stop a new one: of two started
// at once, one takes the lock and replaces SERVE, and the other exits held
func TestAStaleServeStartsOneSidecar(t *testing.T) {
	dir := t.TempDir()
	stale := wire.Published{
		Protocol: wire.Protocol, Server: "dead", PID: 1, Instance: "AAAAAAAAAAAAAAAAAAAAAA",
		Endpoints: []string{"unix:///nowhere/tinystore.sock"},
	}
	text, err := json.Marshal(stale)
	if err != nil {
		t.Fatal(err)
	}
	if err = os.MkdirAll(filepath.Join(dir, "server"), 0o700); err != nil {
		t.Fatal(err)
	}
	if err = os.WriteFile(filepath.Join(dir, "server", "SERVE"), text, 0o600); err != nil {
		t.Fatal(err)
	}

	ctx, cancel := context.WithCancel(t.Context())
	servers := []<-chan error{
		start(ctx, t, "--dir", dir, "--local", "--idle", "0"),
		start(ctx, t, "--dir", dir, "--local", "--idle", "0"),
	}
	var winner <-chan error
	select {
	case err = <-servers[0]:
		winner = servers[1]
	case err = <-servers[1]:
		winner = servers[0]
	case <-time.After(20 * time.Second):
		t.Fatal("neither server found the directory held")
	}
	if !errors.Is(err, errHeld) {
		t.Fatalf("the server that lost: %v", err)
	}
	handshake(t, waitServe(t, dir, stale.Instance, winner))
	cancel()
	if err = ended(t, winner); err != nil {
		t.Fatal(err)
	}
	mustRelease(t, dir)
}

// a private child serves its parent on stdin and stdout, and leaves when the
// parent ends its side
func TestAPrivateChildServesItsParent(t *testing.T) {
	dir := t.TempDir()
	stdin, parentWrites := io.Pipe()
	parentReads, stdout := io.Pipe()
	logs := &lockedBuffer{}
	done := make(chan error, 1)
	go func() {
		done <- serve(t.Context(), []string{"--dir", dir, "--stdio"}, console{stdin: stdin, stdout: stdout, stderr: logs})
	}()
	hello := wire.Hello{Protocol: wire.Protocol, Client: "tinystore-cmd-test"}
	if _, err := parentWrites.Write(wire.AppendFrame(nil, wire.Header{Kind: wire.KindHello}, hello.Append(nil))); err !=
		nil {
		t.Fatal(err)
	}
	if welcome := readWelcome(t, parentReads); welcome.Capability != wire.Admin {
		t.Fatalf("a private child's parent is %s", welcome.Capability)
	}
	if err := parentWrites.Close(); err != nil {
		t.Fatal(err)
	}
	if err := ended(t, done); err != nil {
		t.Fatalf("%v\n%s", err, logs)
	}
	mustRelease(t, dir)
}

// a private child told to leave does, though its parent keeps its side open
// and says nothing: closing a blocking stdin ends no read waiting on it
func TestAPrivateChildLeavesWhenToldThoughItsParentStays(t *testing.T) {
	dir := t.TempDir()
	hello := wire.Hello{Protocol: wire.Protocol, Client: "tinystore-cmd-test"}
	quiet := make(chan struct{})
	defer close(quiet)
	stdin := io.MultiReader(bytes.NewReader(wire.AppendFrame(nil, wire.Header{Kind: wire.KindHello}, hello.Append(nil))),
		silentParent{quiet})
	parentReads, stdout := io.Pipe()
	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan error, 1)
	go func() {
		done <- serve(ctx, []string{"--dir", dir, "--stdio"}, console{stdin: stdin, stdout: stdout, stderr: &lockedBuffer{}})
	}()
	readWelcome(t, parentReads)
	go func() { _, _ = io.Copy(io.Discard, parentReads) }() // the GOAWAY
	cancel()
	if err := ended(t, done); err != nil {
		t.Fatal(err)
	}
	mustRelease(t, dir)
}

// silentParent keeps its side of stdin open and writes nothing, as a parent
// whose pipe's close ends no read waiting on it
type silentParent struct{ quiet <-chan struct{} }

func (p silentParent) Read([]byte) (int, error) {
	<-p.quiet
	return 0, io.EOF
}

// what serve cannot do is refused before it opens anything, a file it cannot
// read included
func TestServeRefusesWhatItCannotServe(t *testing.T) {
	dir, files := t.TempDir(), t.TempDir()
	tokens, missing := filepath.Join(files, "tokens"), filepath.Join(files, "missing")
	token := base64.RawURLEncoding.EncodeToString(make([]byte, 32))
	if err := os.WriteFile(tokens, []byte("admin "+token), 0o600); err != nil {
		t.Fatal(err)
	}
	for _, args := range [][]string{
		{"--stdio"},
		{"--dir", dir},
		{"--dir", dir, "--stdio", "--local"},
		{"--dir", dir, "--listen", "tcp://127.0.0.1:0"},
		{"--dir", dir, "--listen", "tcp://127.0.0.1:0", "--tokens", missing},
		{"--dir", dir, "--listen", "tls://127.0.0.1:0", "--tokens", tokens, "--tls-cert", missing},
		{"--dir", dir, "--listen", "tls://127.0.0.1:0", "--tokens", tokens},
		{"--dir", dir, "--listen", "tls://127.0.0.1:0", "--tokens", tokens, "--tls-cert", missing, "--tls-key", missing},
		{"--dir", dir, "--listen", "tcp://127.0.0.1:0", "--tokens", tokens, "--tls-cert", missing, "--tls-key", missing},
		{"--dir", dir, "--local", "--tls-cert", missing, "--tls-key", missing},
		{"--dir", dir, "--local", "--idle", "-1s"},
		{"--dir", dir, "--local", "extra"},
	} {
		if err := serve(t.Context(), args, console{stderr: &lockedBuffer{}}); err == nil || errors.Is(err, errHeld) {
			t.Errorf("serve %q: %v", args, err)
		}
	}
	if entries, err := os.ReadDir(dir); err != nil || len(entries) != 0 {
		t.Fatalf("a refused serve left %v, %v", entries, err)
	}
}

// A remote server holds 1 GiB of the store's memory unless --memory says
// otherwise, 0 for no bound. A local one is bound only when --memory says.
func TestARemoteServerIsBoundedUnlessToldOtherwise(t *testing.T) {
	dir := t.TempDir()
	for _, c := range []struct {
		args   []string
		memory int64
	}{
		{[]string{"--dir", dir, "--listen", "tcp://127.0.0.1:0", "--tokens", "t"}, 1 << 30},
		{[]string{"--dir", dir, "--listen", "tcp://127.0.0.1:0", "--tokens", "t", "--memory", "0"}, 0},
		{[]string{"--dir", dir, "--local", "--listen", "tcp://127.0.0.1:0", "--tokens", "t", "--memory", "5"}, 5},
		{[]string{"--dir", dir, "--local"}, 0},
		{[]string{"--dir", dir, "--stdio"}, 0},
	} {
		asked, err := parseServe(c.args, &lockedBuffer{})
		if err != nil || asked.memory != c.memory {
			t.Errorf("serve %q holds %d bytes, %v; want %d", c.args, asked.memory, err, c.memory)
		}
	}
}
