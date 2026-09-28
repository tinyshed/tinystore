package server

import (
	"context"
	"errors"
	"fmt"
	"net"
	"strings"
	"sync"
	"testing"

	"github.com/tinyshed/tinystore/server/internal/client"
	"github.com/tinyshed/tinystore/server/wire"
)

// tap records the frames a server sends on a connection
type tap struct {
	net.Conn
	mu     sync.Mutex
	finals map[uint32]int
	buf    []byte
}

func (tp *tap) Read(p []byte) (int, error) {
	n, err := tp.Conn.Read(p)
	tp.mu.Lock()
	defer tp.mu.Unlock()
	tp.buf = append(tp.buf, p[:n]...)
	for len(tp.buf) >= wire.HeaderSize {
		h := wire.ParseHeader(tp.buf)
		if len(tp.buf) < wire.HeaderSize+int(h.Length) {
			break
		}
		if (h.Kind == wire.KindResponse || h.Kind == wire.KindData) && h.Flags&wire.FlagEnd != 0 {
			tp.finals[h.Stream]++
		}
		tp.buf = tp.buf[wire.HeaderSize+int(h.Length):]
	}
	return n, err
}

// every stream ends with one final frame, whatever its handler did: answered,
// failed, panicked, cancelled, left unanswered, downloaded or uploaded
func TestEveryStreamEndsOnce(t *testing.T) {
	ts := startTestServer(t, Options{})
	raw, err := net.Dial("tcp", strings.TrimPrefix(ts.endpoint, "tcp://"))
	if err != nil {
		t.Fatal(err)
	}
	tapped := &tap{Conn: raw, finals: map[uint32]int{}}
	conn, err := client.New(tapped, wire.Hello{})
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()

	kinds := []func(t *testing.T) uint32{
		func(t *testing.T) uint32 { return callExpecting(t, conn, testEcho, wire.Handle{Handle: 3}, "") },
		func(t *testing.T) uint32 { return callExpecting(t, conn, testPanic, wire.Empty{}, wire.CodeInternal) },
		func(t *testing.T) uint32 { return callExpecting(t, conn, testSilent, wire.Empty{}, wire.CodeInternal) },
		func(t *testing.T) uint32 { return callExpecting(t, conn, 0xfe01, wire.Empty{}, wire.CodeUnimplemented) },
		func(t *testing.T) uint32 { return callExpecting(t, conn, testEcho, rawBody{0xc1}, wire.CodeInvalid) },
		func(t *testing.T) uint32 { return cancelled(t, conn, ts) },
		func(t *testing.T) uint32 { return download(t, conn, 40) },
		func(t *testing.T) uint32 { return upload(t, conn, 300<<10) },
	}
	var streams sync.WaitGroup
	ids := make(chan uint32, 8*len(kinds))
	for round := range 8 {
		for i, kind := range kinds {
			streams.Go(func() {
				t.Run(fmt.Sprintf("kind %d round %d", i, round), func(t *testing.T) { ids <- kind(t) })
			})
		}
	}
	streams.Wait()
	close(ids)

	echo(t, conn, 1) // what the server sent before it has been read
	tapped.mu.Lock()
	defer tapped.mu.Unlock()
	for id := range ids {
		if tapped.finals[id] != 1 {
			t.Errorf("stream %d ended %d times", id, tapped.finals[id])
		}
	}
}

// rawBody is a body as it is, a message or not
type rawBody []byte

func (r rawBody) Append(dst []byte) []byte { return append(dst, r...) }

func callExpecting(t *testing.T, conn *client.Conn, method wire.Method, request client.Message, want wire.Code) uint32 {
	t.Helper()
	st, err := conn.Open(t.Context(), method, request, true)
	if err != nil {
		t.Fatal(err)
	}
	_, err = st.Response(t.Context())
	if code := codeOfError(err); code != want {
		t.Errorf("method %#04x: %v, want %q", uint16(method), err, want)
	}
	return st.ID()
}

func cancelled(t *testing.T, conn *client.Conn, ts *testServer) uint32 {
	t.Helper()
	st, err := conn.Open(t.Context(), testHold, wire.Empty{}, true)
	if err != nil {
		t.Fatal(err)
	}
	waitFor(t, "a held call", func() bool { return ts.holding.Load() > 0 })
	if cancelErr := st.Cancel(); cancelErr != nil {
		t.Fatal(cancelErr)
	}
	if _, err = st.Response(t.Context()); codeOfError(err) != wire.CodeCancelled {
		t.Errorf("a cancelled call: %v", err)
	}
	return st.ID()
}

func download(t *testing.T, conn *client.Conn, items uint64) uint32 {
	t.Helper()
	st, err := conn.Open(t.Context(), testDownload, wire.Handle{Handle: items}, true)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := st.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	got := uint64(0)
	for {
		body, last, err := st.Next(t.Context())
		if err != nil {
			t.Fatal(err)
		}
		if last {
			break
		}
		if len(body) < 1000 {
			t.Fatalf("an item of %d bytes", len(body))
		}
		got++
	}
	if got != items {
		t.Errorf("%d items of %d", got, items)
	}
	return st.ID()
}

func upload(t *testing.T, conn *client.Conn, size int) uint32 {
	t.Helper()
	st, err := conn.Open(t.Context(), testUpload, wire.Empty{}, false)
	if err != nil {
		t.Fatal(err)
	}
	chunk := make([]byte, 64<<10)
	for sent := 0; sent < size; sent += len(chunk) {
		chunk = chunk[:min(len(chunk), size-sent)]
		if sendErr := st.Send(t.Context(), chunk, sent+len(chunk) == size); sendErr != nil {
			t.Fatal(sendErr)
		}
	}
	body, err := st.Response(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	var took wire.Handle
	if err := took.Decode(body); err != nil || took.Handle != uint64(size) {
		t.Errorf("the server took %d of %d bytes: %v", took.Handle, size, err)
	}
	return st.ID()
}

// a download stops at the credit the client granted, and goes on as it
// grants more
func TestADownloadWaitsForTheCreditItsClientGrants(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{StreamCredit: 8 << 10})
	st, err := conn.Open(t.Context(), testDownload, wire.Handle{Handle: 100}, true)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := st.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	echo(t, conn, 2) // the connection goes on while the download waits
	items := 0
	for {
		_, last, err := st.Next(t.Context())
		if err != nil {
			t.Fatal(err)
		}
		if last {
			break
		}
		items++
	}
	if items != 100 {
		t.Fatalf("%d items", items)
	}
}

// a stream's number is free the moment its final frame arrives
func TestAStreamNumberIsFreeWhenItsFinalFrameArrives(t *testing.T) {
	ts := startTestServer(t, Options{})
	raw := ts.raw(t, ts.endpoint)
	raw.hello(wire.Hello{})
	for n := range uint64(200) {
		raw.send(wire.Header{Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: testEcho, Stream: 1},
			wire.Handle{Handle: n}.Append(nil))
		h, body, err := raw.next()
		var back wire.Handle
		if err != nil || h.Kind != wire.KindResponse || h.Stream != 1 || back.Decode(body) != nil || back.Handle != n {
			t.Fatalf("call %d on stream 1: %s %v", n, h.Kind, err)
		}
	}
}

func TestAnUploadThatIsCancelledEndsCancelled(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	st, err := conn.Open(t.Context(), testUpload, wire.Empty{}, false)
	if err != nil {
		t.Fatal(err)
	}
	if err := st.Send(t.Context(), []byte("the first part"), false); err != nil {
		t.Fatal(err)
	}
	if err := st.Cancel(); err != nil {
		t.Fatal(err)
	}
	if _, err := st.Response(t.Context()); codeOfError(err) != wire.CodeCancelled {
		t.Fatalf("a cancelled upload: %v", err)
	}
	echo(t, conn, 4)
}

func TestManyCallsInFlightFromManyGoroutines(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	var calls sync.WaitGroup
	failed := make(chan error, 256)
	for g := range 256 {
		calls.Go(func() {
			for i := range 50 {
				n := uint64(g*1000 + i)
				body, err := conn.Call(context.Background(), testEcho, wire.Handle{Handle: n})
				var back wire.Handle
				if err == nil {
					err = back.Decode(body)
				}
				if err == nil && back.Handle != n {
					err = fmt.Errorf("%d came back as %d", n, back.Handle)
				}
				if err != nil {
					failed <- err
					return
				}
			}
		})
	}
	calls.Wait()
	close(failed)
	for err := range failed {
		t.Error(err)
	}
	if err := errors.Join(); err != nil {
		t.Fatal(err)
	}
}
