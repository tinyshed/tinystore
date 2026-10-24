package tinystore_test

// What Open says when it cannot reach a store, through the core in this
// process, a private child and a sidecar: a directory that is a file, a
// binary that is not there, and a sidecar that fails before it serves; and
// an Open or a Connect its caller gives up on.

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/ways"
	"tinyshed.org/tinystore/kv"
)

// standIn makes this test binary stand in for tinystore: started as a
// sidecar, it fails and says why on stderr, writing no log.
const standIn = "TINYSTORE_TEST_STAND_IN"

func TestMain(m *testing.M) {
	if os.Getenv(standIn) != "" {
		fmt.Fprintln(os.Stderr, "tinystore serve: a stand-in that never serves")
		os.Exit(1)
	}
	os.Exit(m.Run())
}

type opening struct {
	name    string
	ready   bool
	options []tinystore.Option
}

// local are the ways a store opens from its directory: every one but a remote server's.
func local() []opening {
	library := os.Getenv("TINYSTORE_LIBRARY")
	binary := ways.Binary()
	return []opening{
		{"the core in this process", library != "", []tinystore.Option{tinystore.Library(library)}},
		{"a private child", binary != "", []tinystore.Option{tinystore.Private(), tinystore.Binary(binary)}},
		{"a sidecar", binary != "", []tinystore.Option{tinystore.Binary(binary)}},
	}
}

// contract: ERR-03, WIRE-01
func TestADirectoryThatIsAFileIsInvalidNamingTheFileInEveryWay(t *testing.T) {
	file := filepath.Join(t.TempDir(), "data")
	if err := os.WriteFile(file, []byte("not a directory"), 0o600); err != nil {
		t.Fatal(err)
	}
	under := filepath.Join(file, "store")
	for _, way := range local() {
		t.Run(way.name, func(t *testing.T) {
			if !way.ready {
				t.Skip("not built here")
			}
			_, err := tinystore.Open(context.Background(), file, way.options...)
			if want := "store " + file + " is a file, not a directory"; !errors.Is(err, tinystore.ErrInvalid) || err.Error() != want {
				t.Fatalf("got %v, want an invalid error %q", err, want)
			}
			_, err = tinystore.Open(context.Background(), under, way.options...)
			if want := "store " + under + ": " + file + " is a file, not a directory"; !errors.Is(err, tinystore.ErrInvalid) || err.Error() != want {
				t.Fatalf("got %v, want an invalid error %q", err, want)
			}
		})
	}
}

// contract: ERR-02
func TestABinaryThatIsNotThereIsNamedForASidecarAndAPrivateChild(t *testing.T) {
	base := t.TempDir()
	missing := filepath.Join(base, "nope")
	if runtime.GOOS == "windows" {
		missing += ".exe"
	}
	dir := filepath.Join(base, "store")
	_, err := tinystore.Open(context.Background(), dir, tinystore.Binary(missing))
	if want := "cannot start the sidecar " + missing + ": not found"; !errors.Is(err, tinystore.ErrClosed) || err.Error() != want {
		t.Fatalf("got %v, want a closed error %q", err, want)
	}
	_, err = tinystore.Open(context.Background(), dir, tinystore.Private(), tinystore.Binary(missing))
	if want := "cannot start the private server " + missing + ": not found"; !errors.Is(err, tinystore.ErrClosed) || err.Error() != want {
		t.Fatalf("got %v, want a closed error %q", err, want)
	}
}

// contract: ERR-02
func TestASidecarThatFailsBeforeItServesSaysWhatItWroteToStderr(t *testing.T) {
	dir := t.TempDir()
	if err := os.MkdirAll(filepath.Join(dir, "server"), 0o700); err != nil {
		t.Fatal(err)
	}
	stale := `{"level":"INFO","msg":"stopped","took":"0.1s"}` + "\n"
	if err := os.WriteFile(filepath.Join(dir, "server", "serve.log"), []byte(stale), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv(standIn, "1")
	stand, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	_, err = tinystore.Open(context.Background(), dir, tinystore.Binary(stand))
	want := "the sidecar exited with 1: tinystore serve: a stand-in that never serves"
	if !errors.Is(err, tinystore.ErrClosed) || err.Error() != want {
		t.Fatalf("got %v, want a closed error %q, not an older run's log", err, want)
	}
}

// contract: ERR-02
func TestAPrivateChildThatExitsBeforeItAnswersSaysWhatItWroteOnce(t *testing.T) {
	t.Setenv(standIn, "1")
	stand, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	_, err = tinystore.Open(context.Background(), t.TempDir(), tinystore.Private(), tinystore.Binary(stand))
	want := "the private server did not start: the private server exited with 1: tinystore serve: a stand-in that never serves"
	if !errors.Is(err, tinystore.ErrClosed) || err.Error() != want {
		t.Fatalf("got %v, want a closed error %q", err, want)
	}
}

// heldAfterItsSidecar is a store whose sidecar was stopped and whose directory
// a private child now holds: its next call dials, and waits.
func heldAfterItsSidecar(t *testing.T, binary, dir string) (store, holder *tinystore.Store) {
	t.Helper()
	ctx := context.Background()
	store, err := tinystore.Open(ctx, dir, tinystore.Binary(binary), tinystore.Idle(time.Second))
	if err != nil {
		t.Fatal(err)
	}
	ways.Must(0, ways.Must(kv.OpenBucket[string](store, "notes")).Set(ctx, "a", "kept"))
	if out, err := exec.Command(binary, "stop", dir).CombinedOutput(); err != nil {
		t.Fatalf("stop: %v: %s", err, out)
	}
	// the stopped sidecar lets go of the directory a moment after its SERVE goes
	ways.Until(t, func() bool {
		if holder == nil {
			holder, _ = tinystore.Open(ctx, dir, tinystore.Private(), tinystore.Binary(binary))
		}
		return holder != nil
	})
	return store, holder
}

// contract: WIRE-11
func TestAnOpenCancelledWhileAnotherHoldsTheDirectoryReturnsAtItsDeadline(t *testing.T) {
	binary := ways.Binary()
	if binary == "" {
		t.Skip("tinystore is not built here")
	}
	dir := t.TempDir()
	holder, err := tinystore.Open(context.Background(), dir, tinystore.Private(), tinystore.Binary(binary))
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	started := time.Now()
	_, err = tinystore.Open(ctx, dir, tinystore.Binary(binary), tinystore.Idle(time.Second))
	took := time.Since(started)
	want := "open " + dir + ": cancelled while another process holds the directory and no sidecar answers: " +
		context.DeadlineExceeded.Error()
	if !errors.Is(err, context.DeadlineExceeded) || err.Error() != want {
		t.Fatalf("got %v, want %q", err, want)
	}
	if took > 1500*time.Millisecond {
		t.Fatalf("Open returned %s after it started, its deadline a second", took)
	}
	// every sidecar the open started found the directory held and left; none starts once it is free
	time.Sleep(time.Second)
	_ = holder.Close()
	time.Sleep(1500 * time.Millisecond)
	if _, err := os.Stat(filepath.Join(dir, "server", "SERVE")); err == nil {
		t.Fatal("a sidecar started for an open that had given up")
	}
}

// contract: WIRE-11
func TestAnOpenCancelledWhileTheServerServeNamesSaysNothingStartsNoSidecarAfter(t *testing.T) {
	binary := ways.Binary()
	if binary == "" {
		t.Skip("tinystore is not built here")
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	go func() {
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			go func() { _, _ = io.Copy(io.Discard, conn) }()
		}
	}()
	dir := t.TempDir()
	if err := os.MkdirAll(filepath.Join(dir, "server"), 0o700); err != nil {
		t.Fatal(err)
	}
	secret := make([]byte, 32)
	_, _ = rand.Read(secret)
	published := ways.Must(json.Marshal(map[string]any{
		"protocol": core.ProtocolVersion, "endpoints": []string{"tcp://" + listener.Addr().String()},
		"secret": base64.RawURLEncoding.EncodeToString(secret), "sidecar": true, "instance": "silent",
	}))
	serve := filepath.Join(dir, "server", "SERVE")
	if err := os.WriteFile(serve, published, 0o600); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()
	_, err = tinystore.Open(ctx, dir, tinystore.Binary(binary), tinystore.Idle(time.Second))
	want := "open " + dir + ": cancelled while the sidecar starts: " + context.DeadlineExceeded.Error()
	if !errors.Is(err, context.DeadlineExceeded) || err.Error() != want {
		t.Fatalf("got %v, want %q", err, want)
	}
	// a sidecar started after the cancel opens its log and publishes itself well within this
	time.Sleep(1500 * time.Millisecond)
	if _, err := os.Stat(filepath.Join(dir, "server", "serve.log")); err == nil {
		_ = exec.Command(binary, "stop", dir).Run()
		t.Fatal("a sidecar started for an open that had given up")
	}
	if now, _ := os.ReadFile(serve); !bytes.Equal(now, published) {
		t.Fatalf("SERVE is now %s", now)
	}
}

// contract: WIRE-11
func TestAConnectCancelledWhileItsServerSaysNothingReturnsAtItsDeadline(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	ended := make(chan struct{})
	go func() {
		conn, err := listener.Accept()
		if err != nil {
			return
		}
		_, _ = io.Copy(io.Discard, conn)
		close(ended)
		_ = conn.Close()
	}()
	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()
	started := time.Now()
	url := "tcp://" + listener.Addr().String()
	_, err = tinystore.Connect(ctx, url, "unused")
	want := "connect " + url + ": cancelled while the server has not answered: " + context.DeadlineExceeded.Error()
	if !errors.Is(err, context.DeadlineExceeded) || err.Error() != want {
		t.Fatalf("got %v, want %q", err, want)
	}
	select {
	case <-ended:
	case <-time.After(time.Second):
		t.Fatal("the socket of a connect that gave up stayed open")
	}
	if took := time.Since(started); took > 1500*time.Millisecond {
		t.Fatalf("Connect let its socket go %s after it started, its deadline half a second", took)
	}
}

// contract: WIRE-11
func TestAStoreClosedWhileItDialsReturnsAtOnceAndItsWaitingCallFailsClosed(t *testing.T) {
	binary := ways.Binary()
	if binary == "" {
		t.Skip("tinystore is not built here")
	}
	dir := t.TempDir()
	store, holder := heldAfterItsSidecar(t, binary, dir)
	defer holder.Close()
	notes := ways.Must(kv.OpenBucket[string](store, "notes"))
	waited := make(chan error, 1)
	go func() {
		_, _, err := notes.Get(context.Background(), "a")
		waited <- err
	}()
	time.Sleep(300 * time.Millisecond)
	started := time.Now()
	_ = store.Close()
	if took := time.Since(started); took > time.Second {
		t.Fatalf("Close returned %s after it was called", took)
	}
	select {
	case err := <-waited:
		if !errors.Is(err, tinystore.ErrClosed) || err.Error() != "the store is closed" {
			t.Fatalf("the waiting call got %v, want a closed error", err)
		}
	case <-time.After(time.Second):
		t.Fatal("the waiting call did not return once the store closed")
	}
}
