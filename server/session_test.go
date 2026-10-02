package server

import (
	"bytes"
	"crypto/rand"
	"encoding/base64"
	"errors"
	"io"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/server/internal/client"
	"github.com/tinyshed/tinystore/server/wire"
)

// rawConn is a connection the test writes frames to and reads frames from
// without a client in between
type rawConn struct {
	t      *testing.T
	conn   net.Conn
	reader *wire.Reader
}

func (ts *testServer) raw(t *testing.T, endpoint string) *rawConn {
	t.Helper()
	conn, err := net.Dial("tcp", strings.TrimPrefix(endpoint, "tcp://"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	return &rawConn{t: t, conn: conn, reader: wire.NewReader(conn, 64<<20)}
}

func (r *rawConn) send(h wire.Header, body []byte) {
	r.t.Helper()
	if _, err := r.conn.Write(wire.AppendFrame(nil, h, body)); err != nil {
		r.t.Fatal(err)
	}
}

// next is the next frame the server sent, or an error once it closed
func (r *rawConn) next() (wire.Header, []byte, error) {
	r.t.Helper()
	_ = r.conn.SetReadDeadline(time.Now().Add(10 * time.Second))
	h, err := r.reader.Header()
	if err != nil {
		return h, nil, err
	}
	body, err := r.reader.Body(nil)
	return h, body, err
}

func (r *rawConn) hello(hello wire.Hello) wire.Welcome {
	r.t.Helper()
	if hello.Protocol == 0 {
		hello.Protocol = wire.Protocol
	}
	r.send(wire.Header{Kind: wire.KindHello}, hello.Append(nil))
	h, body, err := r.next()
	if err != nil || h.Kind != wire.KindWelcome {
		r.t.Fatalf("%s instead of the WELCOME: %v", h.Kind, err)
	}
	var welcome wire.Welcome
	if err := welcome.Decode(body); err != nil {
		r.t.Fatal(err)
	}
	return welcome
}

// goneAway reads until the server's GOAWAY, which it returns, and then the end
// of the connection
func (r *rawConn) goneAway() wire.GoAway {
	r.t.Helper()
	var goAway wire.GoAway
	for {
		h, body, err := r.next()
		if err != nil {
			r.t.Fatalf("the connection ended without a GOAWAY: %v", err)
		}
		if h.Kind != wire.KindGoAway {
			continue
		}
		if err := goAway.Decode(body); err != nil {
			r.t.Fatal(err)
		}
		break
	}
	if _, _, err := r.next(); !errors.Is(err, io.EOF) && !isReset(err) {
		r.t.Fatalf("after the GOAWAY: %v", err)
	}
	return goAway
}

func isReset(err error) bool {
	var netErr net.Error
	return errors.As(err, &netErr) || errors.Is(err, io.ErrUnexpectedEOF) || err != nil &&
		strings.Contains(err.Error(), "reset")
}

func TestTheWelcomeStatesWhatTheConnectionAgreed(t *testing.T) {
	ts := startTestServer(t, Options{Version: "0.0.1"})
	welcome := ts.raw(t, ts.endpoint).hello(wire.Hello{Client: "test", MaxBody: 64 << 10})
	if welcome.Protocol != wire.Protocol || welcome.Server != "0.0.1" || welcome.Capability != wire.Admin ||
		welcome.MaxBody != 64<<10 || welcome.InFlight != 256 || welcome.ConnectionCredit < welcome.MaxBody ||
		welcome.StreamCredit < welcome.MaxBody || !bytes.Equal(welcome.Instance, ts.server.Instance()) ||
		welcome.Now == 0 || len(welcome.Engines) == 0 {
		t.Fatalf("%+v", welcome)
	}
	small := ts.raw(t, ts.endpoint).hello(wire.Hello{StreamCredit: 16 << 10})
	if small.MaxBody != 16<<10 {
		t.Fatalf("a body larger than the stream credit it must fit: %d", small.MaxBody)
	}
}

func TestAHelloTheServerCannotTakeIsAGoAway(t *testing.T) {
	ts := startTestServer(t, Options{})

	first := ts.raw(t, ts.endpoint)
	first.send(wire.Header{Kind: wire.KindPing, Length: 8}, make([]byte, 8))
	if goAway := first.goneAway(); goAway.Code != wire.CodeProtocol {
		t.Errorf("a PING first: %+v", goAway)
	}

	older := ts.raw(t, ts.endpoint)
	older.send(wire.Header{Kind: wire.KindHello}, wire.Hello{Protocol: wire.OldestProtocol - 1}.Append(nil))
	if goAway := older.goneAway(); goAway.Code != wire.CodeProtocol || !strings.Contains(goAway.Message, "protocols 1 to") {
		t.Errorf("a protocol older than the oldest: %+v", goAway)
	}

	garbled := ts.raw(t, ts.endpoint)
	garbled.send(wire.Header{Kind: wire.KindHello}, []byte{0xc1})
	if goAway := garbled.goneAway(); goAway.Code != wire.CodeProtocol {
		t.Errorf("a HELLO that is not a message: %+v", goAway)
	}
}

// A client newer than its server speaks the server's protocol: the server
// answers a HELLO of a newer protocol with its own newest, which the client
// speaks too or closes, rather than refusing a connection both could have had.
func TestAClientOfANewerProtocolIsWelcomedInTheServers(t *testing.T) {
	ts := startTestServer(t, Options{})
	welcome := ts.raw(t, ts.endpoint).hello(wire.Hello{Protocol: wire.Protocol + 1, Client: "tomorrow"})
	if welcome.Protocol != wire.Protocol {
		t.Fatalf("a client of protocol %d welcomed in %d", wire.Protocol+1, welcome.Protocol)
	}
}

// A request is understood whole or refused. A field the server skipped could
// change what the client asked, as a newer client's condition on a read would,
// so the server answers unimplemented, naming the field and its own version,
// rather than an answer to another question.
func TestARequestWithAFieldTheServerDoesNotKnowIsRefused(t *testing.T) {
	ts := startTestServer(t, Options{Version: "v0.0.1"})
	raw := ts.raw(t, ts.endpoint)
	raw.hello(wire.Hello{Client: "tomorrow"})

	m := wire.BeginMap(nil)
	m.Str(1, "notes")
	m.Uint(99, 1)
	raw.send(wire.Header{Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: wire.KVOpen, Stream: 1}, m.End())
	h, body, err := raw.next()
	if err != nil || h.Flags&wire.FlagError == 0 {
		t.Fatalf("%+v answered a request with field 99: %v", h, err)
	}
	var failed wire.Error
	if err = failed.Decode(body); err != nil {
		t.Fatal(err)
	}
	if failed.Code != wire.CodeUnimplemented || failed.What["field"] != "99" || !strings.Contains(failed.Message, "v0.0.1") {
		t.Fatalf("refused as %+v", failed)
	}

	opened := ts.raw(t, ts.endpoint)
	opened.hello(wire.Hello{Client: "today"})
	opened.send(wire.Header{Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: wire.KVOpen, Stream: 1},
		wire.KVBucket{Name: "notes"}.Append(nil))
	if h, _, err = opened.next(); err != nil || h.Flags&wire.FlagError != 0 {
		t.Fatalf("the same request without field 99: %+v, %v", h, err)
	}
}

func newToken(t *testing.T) string {
	t.Helper()
	secret := make([]byte, tokenBytes)
	if _, err := rand.Read(secret); err != nil {
		t.Fatal(err)
	}
	return base64.RawURLEncoding.EncodeToString(secret)
}

// a remote connection comes in with a token, and its capability is the
// token's line
func TestARemoteConnectionNeedsItsToken(t *testing.T) {
	admin, data := newToken(t), newToken(t)
	tokens, err := ParseTokens([]byte("# who may connect\nadmin " + admin + "\n\ndata  " + data + "\n"))
	if err != nil {
		t.Fatal(err)
	}
	ts := startTestServer(t, Options{Tokens: tokens})

	if goAway := ts.hello(t, ts.remote, wire.Hello{}); goAway.Code != wire.CodeUnauthenticated {
		t.Errorf("no token: %+v", goAway)
	}
	if goAway := ts.hello(t, ts.remote, wire.Hello{Token: newToken(t)}); goAway.Code != wire.CodeUnauthenticated {
		t.Errorf("an unknown token: %+v", goAway)
	}
	if welcome := ts.raw(t, ts.remote).hello(wire.Hello{Token: data}); welcome.Capability != wire.Data {
		t.Errorf("a data token: %s", welcome.Capability)
	}
	if welcome := ts.raw(t, ts.remote).hello(wire.Hello{Token: admin}); welcome.Capability != wire.Admin {
		t.Errorf("an admin token: %s", welcome.Capability)
	}
}

func (ts *testServer) hello(t *testing.T, endpoint string, hello wire.Hello) wire.GoAway {
	t.Helper()
	raw := ts.raw(t, endpoint)
	hello.Protocol = wire.Protocol
	raw.send(wire.Header{Kind: wire.KindHello}, hello.Append(nil))
	return raw.goneAway()
}

func TestATokensFileRefusesWhatIsNotAToken(t *testing.T) {
	for _, text := range []string{
		"admin short",
		"root " + base64.RawURLEncoding.EncodeToString(make([]byte, 32)),
		"admin " + base64.RawURLEncoding.EncodeToString(make([]byte, 32)) + " extra",
		"data " + base64.StdEncoding.EncodeToString(make([]byte, 32)),
	} {
		if _, err := ParseTokens([]byte(text)); err == nil {
			t.Errorf("taken: %q", text)
		}
	}
}

func TestAConnectionWithoutAHelloEndsWithTheHandshakesTime(t *testing.T) {
	ts := startTestServer(t, Options{}, func(l *limits) { l.handshake = 50 * time.Millisecond })
	raw := ts.raw(t, ts.endpoint)
	began := time.Now()
	if _, _, err := raw.next(); err == nil {
		t.Fatal("a frame before the HELLO")
	}
	if waited := time.Since(began); waited > 5*time.Second {
		t.Fatalf("closed after %v", waited)
	}
}

// a client past its credit loses its connection, and the reader goes on
// taking frames while every handler waits
func TestAClientPastItsCreditIsCutOff(t *testing.T) {
	ts := startTestServer(t, Options{}, func(l *limits) { l.connectionCredit = 64 << 10 })
	raw := ts.raw(t, ts.endpoint)
	welcome := raw.hello(wire.Hello{})
	if welcome.ConnectionCredit != 64<<10 {
		t.Fatalf("credit %d", welcome.ConnectionCredit)
	}

	padding := wire.KVCall{Handle: 1, Key: strings.Repeat("k", 1000)}.Append(nil)
	sent := 0
	for stream := uint32(1); sent+len(padding) <= 64<<10; stream++ {
		raw.send(wire.Header{Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: testHold, Stream: stream}, padding)
		sent += len(padding)
	}
	waitFor(t, "the held calls", func() bool { return ts.holding.Load() == int64(sent/len(padding)) })

	raw.send(wire.Header{Kind: wire.KindPing, Length: 8}, []byte("pingpong"))
	if h, body, err := raw.next(); err != nil || h.Kind != wire.KindPong || string(body) != "pingpong" {
		t.Fatalf("the reader stopped while handlers wait: %s %q %v", h.Kind, body, err)
	}

	raw.send(wire.Header{Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: testEcho, Stream: 9999}, padding)
	if goAway := raw.goneAway(); goAway.Code != wire.CodeProtocol || !strings.Contains(goAway.Message, "credit") {
		t.Fatalf("past the credit: %+v", goAway)
	}
}

func TestFramesThatBreakTheProtocolEndTheConnection(t *testing.T) {
	ts := startTestServer(t, Options{})
	cases := []struct {
		name   string
		frames func(r *rawConn)
	}{
		{"a stream in use", func(r *rawConn) {
			r.send(wire.Header{Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: testHold, Stream: 5}, wire.Empty{}.Append(nil))
			r.send(wire.Header{Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: testHold, Stream: 5}, wire.Empty{}.Append(nil))
		}},
		{"a second HELLO", func(r *rawConn) {
			r.send(wire.Header{Kind: wire.KindHello}, wire.Hello{Protocol: 1}.Append(nil))
		}},
		{"a RESPONSE from a client", func(r *rawConn) {
			r.send(wire.Header{Kind: wire.KindResponse, Flags: wire.FlagEnd, Stream: 3}, wire.Empty{}.Append(nil))
		}},
		{"DATA on a call", func(r *rawConn) {
			r.send(wire.Header{Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: testHold, Stream: 6}, wire.Empty{}.Append(nil))
			r.send(wire.Header{Kind: wire.KindData, Stream: 6}, []byte("x"))
		}},
		{"an empty DATA that does not end", func(r *rawConn) {
			r.send(wire.Header{Kind: wire.KindRequest, Method: testUpload, Stream: 7}, wire.Empty{}.Append(nil))
			r.send(wire.Header{Kind: wire.KindData, Stream: 7}, nil)
		}},
		{"DATA past its stream's credit", func(r *rawConn) {
			r.send(wire.Header{Kind: wire.KindRequest, Method: testHold, Stream: 8}, wire.Empty{}.Append(nil))
			chunk := make([]byte, 64<<10)
			for range 3<<20/len(chunk) + 1 {
				r.send(wire.Header{Kind: wire.KindData, Stream: 8}, chunk)
			}
		}},
		{"a client's CREDIT for the connection", func(r *rawConn) {
			r.send(wire.Header{Kind: wire.KindCredit, Length: 4}, []byte{1, 0, 0, 0})
		}},
		{"a kind nobody defined", func(r *rawConn) {
			r.send(wire.Header{Kind: 11}, nil)
		}},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			raw := ts.raw(t, ts.endpoint)
			raw.hello(wire.Hello{})
			c.frames(raw)
			if goAway := raw.goneAway(); goAway.Code != wire.CodeProtocol {
				t.Fatalf("%+v", goAway)
			}
		})
	}
}

// a quiet client is asked with a PING, and closed when it stays quiet past it;
// one that answers stays
func TestAQuietClientIsPingedAndThenClosed(t *testing.T) {
	ts := startTestServer(t, Options{}, func(l *limits) { l.silence = 50 * time.Millisecond })

	quiet := ts.raw(t, ts.endpoint)
	quiet.hello(wire.Hello{})
	if h, _, err := quiet.next(); err != nil || h.Kind != wire.KindPing {
		t.Fatalf("%s instead of a PING: %v", h.Kind, err)
	}
	if _, _, err := quiet.next(); err == nil {
		t.Fatal("a quiet client stayed")
	}

	answering := ts.dial(t, wire.Hello{})
	time.Sleep(300 * time.Millisecond)
	echo(t, answering, 5)
}

// the end of what a client sends, as a parent closing a child's stdin, drains
// the streams running: their answers are written before the server closes
func TestTheEndOfWhatAClientSendsDrainsItsStreams(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	var streams []*client.Stream
	for range 20 {
		st, err := conn.Open(t.Context(), testHold, wire.Empty{}, true)
		if err != nil {
			t.Fatal(err)
		}
		streams = append(streams, st)
	}
	waitFor(t, "the held calls", func() bool { return ts.holding.Load() == 20 })
	if err := conn.CloseWrite(); err != nil {
		t.Fatal(err)
	}
	ts.letGo()
	for _, st := range streams {
		if _, err := st.Response(t.Context()); err != nil {
			t.Fatal(err)
		}
	}
	<-conn.Done()
}

// a lost connection leaves each piece of work as a crash would: an upload in
// flight leaves nothing, and a job in the worker's hands fails that attempt
func TestALostConnectionAbortsUploadsAndFailsAttemptsInHand(t *testing.T) {
	ts := startTestServer(t, Options{})
	observer := ts.dial(t, wire.Hello{})
	lost := ts.dial(t, wire.Hello{})

	exports := openBlobs(t, lost, wire.BlobsBucket{Name: "exports"})
	upload, err := lost.Open(t.Context(), wire.BlobsPut, wire.BlobsCall{Handle: exports, Key: "half.zip", Size: -1},
		false)
	if err != nil {
		t.Fatal(err)
	}
	if sendErr := upload.Send(t.Context(), randomBytes(t, 100<<10), false); sendErr != nil {
		t.Fatal(sendErr)
	}

	queue := openQueue(t, lost, wire.JobsQueue{Name: "mails"})
	if enqueueErr := enqueueJobs(t, lost, queue, wire.JobsJob{Value: `{"to":"a"}`, Key: "a"}); enqueueErr != nil {
		t.Fatal(enqueueErr)
	}
	work, err := lost.Open(t.Context(), wire.JobsWork, wire.JobsWorkers{Handle: queue}, false)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := work.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	nextHeld(t, work)
	_ = lost.Close()

	mails := openQueue(t, observer, wire.JobsQueue{Name: "mails"})
	waitFor(t, "the attempt in hand to fail", func() bool {
		return fetchJob(t, observer, mails, "a").Err != ""
	})
	if job := fetchJob(t, observer, mails, "a"); job.Attempt != 1 || job.State != 1 {
		t.Fatalf("the job in hand: %+v", job)
	}
	waitFor(t, "the upload to leave nothing", func() bool {
		entries, err := os.ReadDir(filepath.Join(ts.root, "blobs", "uploads"))
		return err == nil && len(entries) == 0
	})
	observed := openBlobs(t, observer, wire.BlobsBucket{Name: "exports"})
	if object, err := blobsDo(t, observer, wire.BlobsStat, wire.BlobsCall{
		Handle: observed, Key: "half.zip",
		Size: -1,
	}); err != nil || object.Found {
		t.Fatalf("the upload left %+v, %v", object, err)
	}
}
