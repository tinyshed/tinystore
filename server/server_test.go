package server

import (
	"context"
	"errors"
	"net"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/server/internal/client"
	"github.com/tinyshed/tinystore/server/wire"
)

// methods only the tests serve, under a high byte no engine takes
const (
	testEcho     wire.Method = 0xff01 // answers its Handle with itself
	testHold     wire.Method = 0xff02 // waits until the test lets go, or its stream's context ends
	testDownload wire.Method = 0xff03 // sends Handle items of a kilobyte, then a trailer
	testUpload   wire.Method = 0xff04 // takes every DATA and answers with the bytes it took
	testPanic    wire.Method = 0xff05
	testSilent   wire.Method = 0xff06 // returns without answering
)

type testServer struct {
	root     string // the store's directory
	store    *tinystore.Store
	server   *Server
	endpoint string
	remote   string // a listener whose connections carry tokens
	held     chan struct{}
	holding  atomic.Int64
}

// startTestServer serves a Manual store on loopback TCP, one listener taken as
// local and one as remote, with the test methods beside the engines'
func startTestServer(t *testing.T, options Options, change ...func(*limits)) *testServer {
	t.Helper()
	ctx := context.Background()
	root := t.TempDir()
	store, err := tinystore.Open(ctx, root, tinystore.Options{Manual: true})
	if err != nil {
		t.Fatal(err)
	}
	server, err := New(store, options)
	if err != nil {
		t.Fatal(err)
	}
	for _, c := range change {
		c(&server.limits)
	}
	ts := &testServer{root: root, store: store, server: server, held: make(chan struct{})}
	ts.addTestMethods()

	serving := make(chan struct{}, 2)
	for _, remote := range []bool{false, true} {
		l, err := net.Listen("tcp", "127.0.0.1:0")
		if err != nil {
			t.Fatal(err)
		}
		listener := netListener{listener: l, endpoint: "tcp://" + l.Addr().String(), remote: remote}
		if remote {
			ts.remote = listener.endpoint
		} else {
			ts.endpoint = listener.endpoint
		}
		go func() {
			if err := server.Serve(ctx, listener); err != nil {
				t.Errorf("serve: %v", err)
			}
			serving <- struct{}{}
		}()
	}
	t.Cleanup(func() {
		ts.letGo()
		if err := server.Close(ctx); err != nil {
			t.Errorf("close: %v", err)
		}
		<-serving
		<-serving
		if err := store.Close(ctx); err != nil {
			t.Errorf("close the store: %v", err)
		}
	})
	return ts
}

func (ts *testServer) letGo() {
	select {
	case <-ts.held:
	default:
		close(ts.held)
	}
}

func (ts *testServer) addTestMethods() {
	methods := ts.server.methods
	methods[testEcho] = func(c *call) error {
		var ask wire.Handle
		if err := ask.Decode(c.request); err != nil {
			return err
		}
		return respond(c, ask)
	}
	methods[testHold] = func(c *call) error {
		ts.holding.Add(1)
		defer ts.holding.Add(-1)
		select {
		case <-ts.held:
			return respond(c, wire.Empty{})
		case <-c.ctx.Done():
			return context.Cause(c.ctx)
		}
	}
	methods[testDownload] = testDownloadMethod
	methods[testUpload] = testUploadMethod
	methods[testPanic] = func(*call) error { panic("a handler's own fault") }
	methods[testSilent] = func(*call) error { return nil }
}

func testDownloadMethod(c *call) error {
	var ask wire.Handle
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	if err := begin(c, wire.Empty{}); err != nil {
		return err
	}
	kilobyte := wire.KVEntry{Value: wire.KVValue{Kind: wire.KVBytes, Bytes: make([]byte, 1000)}}
	for range ask.Handle {
		if err := item(c, kilobyte); err != nil {
			return err
		}
	}
	return trailer(c, wire.KVPage{})
}

func testUploadMethod(c *call) error {
	total := uint64(0)
	for {
		body, last, err := c.receive()
		if err != nil {
			return err
		}
		total += uint64(len(body))
		c.consumed(body)
		if last {
			return respond(c, wire.Handle{Handle: total})
		}
	}
}

func (ts *testServer) dial(t *testing.T, hello wire.Hello) *client.Conn {
	t.Helper()
	return ts.dialAt(t, ts.endpoint, hello)
}

func (ts *testServer) dialAt(t *testing.T, endpoint string, hello wire.Hello) *client.Conn {
	t.Helper()
	conn, err := client.Dial(t.Context(), endpoint, hello)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	return conn
}

// waitFor polls a condition the test cannot be told of otherwise
func waitFor(t *testing.T, what string, done func() bool) {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for !done() {
		if time.Now().After(deadline) {
			t.Fatalf("still waiting for %s", what)
		}
		time.Sleep(time.Millisecond)
	}
}

func codeOfError(err error) wire.Code {
	var failure *wire.Error
	if errors.As(err, &failure) {
		return failure.Code
	}
	return ""
}

func echo(t *testing.T, conn *client.Conn, n uint64) {
	t.Helper()
	body, err := conn.Call(t.Context(), testEcho, wire.Handle{Handle: n})
	if err != nil {
		t.Fatal(err)
	}
	var back wire.Handle
	if err := back.Decode(body); err != nil || back.Handle != n {
		t.Fatalf("echoed %d as %d, %v", n, back.Handle, err)
	}
}

// the server says GOAWAY to each connection when it closes: the streams
// running finish, what crosses the GOAWAY is answered unavailable, and the
// connection closes after its last stream
func TestClosingTheServerLetsTheStreamsRunningFinish(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	held, err := conn.Open(t.Context(), testHold, wire.Empty{}, true)
	if err != nil {
		t.Fatal(err)
	}
	waitFor(t, "the held call", func() bool { return ts.holding.Load() == 1 })

	closed := make(chan error, 1)
	go func() { closed <- ts.server.Close(context.Background()) }()
	waitFor(t, "the GOAWAY", func() bool { return conn.GoAway() != nil })
	if code := conn.GoAway().Code; code != wire.CodeUnavailable {
		t.Fatalf("GOAWAY with %s", code)
	}

	crossed := wire.AppendFrame(nil, wire.Header{
		Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: testEcho,
		Stream: 99,
	}, wire.Handle{Handle: 7}.Append(nil))
	if err := conn.SendFrame(crossed); err != nil {
		t.Fatal(err)
	}

	ts.letGo()
	if _, err := held.Response(t.Context()); err != nil {
		t.Fatalf("the held call: %v", err)
	}
	if err := <-closed; err != nil {
		t.Fatal(err)
	}
	<-conn.Done()
}

// a GOAWAY's crossing request is answered unavailable without running
func TestARequestThatCrossesTheGoAwayIsAnsweredUnavailable(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	held, err := conn.Open(t.Context(), testHold, wire.Empty{}, true)
	if err != nil {
		t.Fatal(err)
	}
	waitFor(t, "the held call", func() bool { return ts.holding.Load() == 1 })
	var session *session
	ts.server.mu.Lock()
	for joined := range ts.server.sessions {
		session = joined
	}
	ts.server.mu.Unlock()
	session.goAway()
	waitFor(t, "the GOAWAY", func() bool { return conn.GoAway() != nil })

	raw := wire.AppendFrame(nil, wire.Header{
		Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: testEcho,
		Stream: 99,
	}, wire.Handle{Handle: 7}.Append(nil))
	tap := make(chan wire.Error, 1)
	go func() {
		// the client knows no stream 99, so the answer is read from what the server sent
		st, err := conn.Adopt(99)
		if err != nil {
			t.Error(err)
			return
		}
		_, err = st.Response(t.Context())
		var failure *wire.Error
		if errors.As(err, &failure) {
			tap <- *failure
		}
		close(tap)
	}()
	if err := conn.SendFrame(raw); err != nil {
		t.Fatal(err)
	}
	failure, ok := <-tap
	if !ok || failure.Code != wire.CodeUnavailable {
		t.Fatalf("the crossing request: %+v", failure)
	}
	ts.letGo()
	if _, err := held.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
}

func TestAConnectionPastTheBoundIsRefused(t *testing.T) {
	ts := startTestServer(t, Options{}, func(l *limits) { l.localSessions = 1 })
	ts.dial(t, wire.Hello{})
	_, err := client.Dial(t.Context(), ts.endpoint, wire.Hello{})
	if code := codeOfError(err); code != wire.CodeLimit {
		t.Fatalf("a second connection: %v", err)
	}
}
