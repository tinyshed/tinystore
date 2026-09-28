package spike

import (
	"context"
	"crypto/rand"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"runtime"
	"runtime/debug"
	"runtime/pprof"
	"sync"
	"sync/atomic"
	"testing"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/kv"
)

// the test binary is also the sidecar: this variable holds its configuration
const rpcSidecarVariable = "TINYSTORE_RPC_SIDECAR"

const (
	rpcKeys      = 10_000
	rpcValueSize = 100
)

// rpcSidecarConfig is what a sidecar serves and how it writes
type rpcSidecarConfig struct {
	Transport string `json:"transport"` // tcp, unix, stdio or pipe
	Address   string `json:"address"`
	Writer    string `json:"writer"` // naive, goroutine or leader
	Pooled    bool   `json:"pooled"` // bodies and answers from pools
	Dir       string `json:"dir"`    // where its store lives
	Profile   string `json:"profile"`
	Workers   int    `json:"workers"`   // calls taken by this many goroutines a connection; zero starts one a call
	GCPercent int    `json:"gcPercent"` // the sidecar's own GOGC; zero keeps the default
}

func TestMain(m *testing.M) {
	if config := os.Getenv(rpcSidecarVariable); config != "" {
		if err := runRPCSidecar(config); err != nil {
			fmt.Fprintln(os.Stderr, "sidecar:", err)
			os.Exit(1)
		}
		os.Exit(0)
	}
	os.Exit(m.Run())
}

func runRPCSidecar(text string) error {
	var config rpcSidecarConfig
	if err := json.Unmarshal([]byte(text), &config); err != nil {
		return err
	}
	if config.Pooled && config.Writer == "goroutine" {
		return errors.New("a goroutine writer keeps what it is sent, so nothing it sends can go back to a pool")
	}
	if config.GCPercent != 0 {
		debug.SetGCPercent(config.GCPercent)
	}
	ctx := context.Background()
	store, bucket, err := openRPCBucket(ctx, config.Dir)
	if err != nil {
		return err
	}
	defer store.Close(ctx)

	server := &rpcServer{bucket: bucket, config: config}
	if config.Pooled {
		server.bodies = &rpcBodies{}
	}
	if config.Transport == "stdio" {
		server.serve(rpcStdio{}) // the frames are the lifeline: their end is the parent's
		return nil
	}
	return server.listen()
}

func (s *rpcServer) listen() error {
	listener, err := rpcListen(s.config.Transport, s.config.Address)
	if err != nil {
		return err
	}
	fmt.Println("ready", listener.address())
	var parentGone atomic.Bool
	go func() {
		_, _ = io.Copy(io.Discard, os.Stdin) // the parent's lifeline
		parentGone.Store(true)
		_ = listener.Close()
	}()
	for {
		conn, err := listener.accept()
		if err != nil {
			return rpcAcceptEnded(err, parentGone.Load())
		}
		go s.serve(conn)
	}
}

// rpcAcceptEnded is nothing when the parent's end closed the listener, the
// way a sidecar's life ends, and the error otherwise
func rpcAcceptEnded(err error, parentGone bool) error {
	if parentGone {
		return nil
	}
	return err
}

// openRPCBucket opens a store in dir and a bucket of rpcKeys values
func openRPCBucket(ctx context.Context, dir string) (*tinystore.Store, *kv.Bucket[[]byte], error) {
	store, err := tinystore.Open(ctx, dir, tinystore.Options{})
	if err != nil {
		return nil, nil, err
	}
	state, err := kv.Open(ctx, store, kv.Options{})
	if err != nil {
		return nil, nil, errors.Join(err, store.Close(ctx))
	}
	bucket, err := kv.OpenBucket[[]byte](ctx, state, "b")
	if err != nil {
		return nil, nil, errors.Join(err, store.Close(ctx))
	}
	if err := fillRPCBucket(ctx, bucket); err != nil {
		return nil, nil, errors.Join(err, store.Close(ctx))
	}
	return store, bucket, nil
}

func fillRPCBucket(ctx context.Context, bucket *kv.Bucket[[]byte]) error {
	var filling sync.WaitGroup
	var failed atomic.Pointer[error]
	for w := range 64 {
		filling.Go(func() {
			value := make([]byte, rpcValueSize)
			for i := w; i < rpcKeys; i += 64 {
				_, _ = rand.Read(value)
				if err := bucket.Set(ctx, rpcKey(i), value); err != nil {
					failed.Store(&err)
				}
			}
		})
	}
	filling.Wait()
	if err := failed.Load(); err != nil {
		return *err
	}
	return nil
}

func rpcKey(i int) string { return fmt.Sprintf("k%05d", i) }

// rpcServer answers the probe's operations from one kv bucket
type rpcServer struct {
	bucket   *kv.Bucket[[]byte]
	config   rpcSidecarConfig
	bodies   *rpcBodies
	answers  sync.Pool
	requests atomic.Int64
	profile  *os.File
}

func (s *rpcServer) serve(conn io.ReadWriteCloser) {
	defer conn.Close()
	w, err := newRPCWriter(s.config.Writer, conn)
	if err != nil {
		fmt.Fprintln(os.Stderr, "sidecar:", err)
		return
	}
	reader := newRPCReader(conn, s.bodies)
	streams := &rpcServerStreams{inboxes: map[uint32]chan rpcFrame{}, allowances: map[uint32]*rpcAllowance{}}
	var handlers sync.WaitGroup
	calls := s.startWorkers(w, &handlers)
	for {
		f, err := reader.next()
		if err != nil {
			break
		}
		switch f.kind {
		case rpcRequest:
			s.dispatch(w, f, streams, &handlers, calls)
		case rpcData:
			streams.deliver(f)
		case rpcCredit:
			streams.grant(f)
		}
	}
	streams.endAll()
	if calls != nil {
		close(calls)
	}
	handlers.Wait()
	w.close()
}

// startWorkers starts Workers goroutines that take calls for as long as the
// connection lasts, keeping the stacks a call into SQLite grew; none leaves a
// goroutine to each call
func (s *rpcServer) startWorkers(w rpcWriter, handlers *sync.WaitGroup) chan rpcFrame {
	if s.config.Workers == 0 {
		return nil
	}
	calls := make(chan rpcFrame, 1024)
	for range s.config.Workers {
		handlers.Go(func() {
			for f := range calls {
				s.call(w, f)
			}
		})
	}
	return calls
}

func (s *rpcServer) dispatch(w rpcWriter, f rpcFrame, streams *rpcServerStreams, handlers *sync.WaitGroup,
	calls chan rpcFrame,
) {
	s.requests.Add(1)
	switch f.method {
	case rpcUpload:
		window := int64(binary.LittleEndian.Uint32(f.body))
		inbox := make(chan rpcFrame, window/rpcChunk+2)
		streams.addInbox(f.stream, inbox)
		handlers.Go(func() { s.upload(w, f, window, inbox) })
	case rpcDownload:
		total := int64(binary.LittleEndian.Uint64(f.body))
		allowance := newRPCAllowance(int64(binary.LittleEndian.Uint32(f.body[8:])))
		f.release()
		streams.addAllowance(f.stream, allowance)
		handlers.Go(func() {
			s.download(w, f.stream, total, allowance)
			streams.removeAllowance(f.stream)
		})
	default:
		if calls != nil {
			calls <- f
			return
		}
		handlers.Go(func() { s.call(w, f) })
	}
}

func (s *rpcServer) call(w rpcWriter, f rpcFrame) {
	ctx := context.Background()
	var answer []byte
	switch f.method {
	case rpcEcho:
		answer = f.body
	case rpcGet:
		answer, _, _ = s.bucket.Get(ctx, string(f.body))
	case rpcSet:
		n := int(f.body[0])
		_ = s.bucket.Set(ctx, string(f.body[1:1+n]), f.body[1+n:])
	case rpcStats:
		answer = s.stats()
	case rpcProfileStart:
		s.startProfile()
	case rpcProfileStop:
		s.stopProfile()
	}
	s.answer(w, f.stream, answer)
	f.release()
}

// answer frames what a call returns: in a buffer of the pool when the writer
// copies what it is sent, a buffer of its own otherwise
func (s *rpcServer) answer(w rpcWriter, stream uint32, body []byte) {
	if s.bodies == nil {
		_ = w.send(rpcAppendFrame(nil, rpcResponse, rpcEnd, 0, stream, body))
		return
	}
	held, _ := s.answers.Get().(*[]byte)
	if held == nil {
		fresh := make([]byte, 0, rpcHeaderSize+rpcPooledBody)
		held = &fresh
	}
	*held = rpcAppendFrame((*held)[:0], rpcResponse, rpcEnd, 0, stream, body)
	_ = w.send(*held)
	s.answers.Put(held)
}

// upload consumes a stream the client sends, granting credit as it goes
func (s *rpcServer) upload(w rpcWriter, request rpcFrame, window int64, inbox chan rpcFrame) {
	stream := request.stream
	request.release()
	consumed := rpcConsumed{window: window}
	var total int64
	for part := range inbox {
		size, end := int64(len(part.body)), part.flags&rpcEnd != 0
		part.release()
		total += size
		if end {
			break
		}
		if grant := consumed.add(size); grant > 0 {
			_ = w.send(rpcCreditFrame(stream, grant))
		}
	}
	_ = w.send(rpcAppendFrame(nil, rpcResponse, rpcEnd, 0, stream, binary.LittleEndian.AppendUint64(nil, uint64(total))))
}

// download sends total bytes in chunks, each within the client's credit
func (s *rpcServer) download(w rpcWriter, stream uint32, total int64, allowance *rpcAllowance) {
	chunk := make([]byte, rpcChunk)
	var frame []byte
	for sent := int64(0); sent < total; {
		n := min(int64(rpcChunk), total-sent)
		if allowance.take(n) != nil {
			return
		}
		sent += n
		flags := byte(0)
		if sent == total {
			flags = rpcEnd
		}
		if !rpcWriterCopies(w) {
			frame = nil
		}
		frame = rpcAppendFrame(frame[:0], rpcData, flags, 0, stream, chunk[:n])
		_ = w.send(frame)
	}
}

func rpcWriterCopies(w rpcWriter) bool {
	_, goroutine := w.(*rpcGoroutineWriter)
	return !goroutine
}

func (s *rpcServer) stats() []byte {
	var m runtime.MemStats
	runtime.ReadMemStats(&m)
	stats, _ := json.Marshal(rpcStatsAnswer{
		Mallocs: m.Mallocs, Bytes: m.TotalAlloc, GC: m.NumGC, Requests: s.requests.Load(),
	})
	return stats
}

// rpcStatsAnswer is the sidecar's own count of what its calls allocated
type rpcStatsAnswer struct {
	Mallocs  uint64 `json:"mallocs"`
	Bytes    uint64 `json:"bytes"`
	GC       uint32 `json:"gc"`
	Requests int64  `json:"requests"`
}

func (s *rpcServer) startProfile() {
	file, err := os.Create(s.config.Profile)
	if err != nil {
		fmt.Fprintln(os.Stderr, "sidecar:", err)
		return
	}
	if err := pprof.StartCPUProfile(file); err != nil {
		fmt.Fprintln(os.Stderr, "sidecar:", err)
		_ = file.Close()
		return
	}
	s.profile = file
}

func (s *rpcServer) stopProfile() {
	if s.profile == nil {
		return
	}
	pprof.StopCPUProfile()
	_ = s.profile.Close()
	s.profile = nil
}

// rpcServerStreams routes a stream's DATA to its handler and its CREDIT to
// what it sends
type rpcServerStreams struct {
	mu         sync.Mutex
	inboxes    map[uint32]chan rpcFrame
	allowances map[uint32]*rpcAllowance
}

func (r *rpcServerStreams) addInbox(stream uint32, inbox chan rpcFrame) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.inboxes[stream] = inbox
}

func (r *rpcServerStreams) addAllowance(stream uint32, allowance *rpcAllowance) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.allowances[stream] = allowance
}

func (r *rpcServerStreams) removeAllowance(stream uint32) {
	r.mu.Lock()
	defer r.mu.Unlock()
	delete(r.allowances, stream)
}

// deliver never waits: a client within its credit fits the inbox
func (r *rpcServerStreams) deliver(f rpcFrame) {
	r.mu.Lock()
	inbox := r.inboxes[f.stream]
	if f.flags&rpcEnd != 0 {
		delete(r.inboxes, f.stream)
	}
	r.mu.Unlock()
	if inbox == nil {
		f.release()
		return
	}
	inbox <- f
	if f.flags&rpcEnd != 0 {
		close(inbox)
	}
}

func (r *rpcServerStreams) grant(f rpcFrame) {
	n := int64(binary.LittleEndian.Uint32(f.body))
	f.release()
	r.mu.Lock()
	allowance := r.allowances[f.stream]
	r.mu.Unlock()
	if allowance != nil {
		allowance.grant(n)
	}
}

func (r *rpcServerStreams) endAll() {
	r.mu.Lock()
	defer r.mu.Unlock()
	for stream, inbox := range r.inboxes {
		close(inbox)
		delete(r.inboxes, stream)
	}
	for _, allowance := range r.allowances {
		allowance.end()
	}
}

// rpcStdio is the parent's pipes as one connection
type rpcStdio struct{}

func (rpcStdio) Read(p []byte) (int, error)  { return os.Stdin.Read(p) }
func (rpcStdio) Write(p []byte) (int, error) { return os.Stdout.Write(p) }
func (rpcStdio) Close() error                { return os.Stdout.Close() }

// rpcListener is a transport's listener as the sidecar uses it
type rpcListener interface {
	accept() (io.ReadWriteCloser, error)
	address() string
	Close() error
}

func rpcListen(transport, address string) (rpcListener, error) {
	switch transport {
	case "tcp", "unix":
		listener, err := net.Listen(transport, address)
		if err != nil {
			return nil, err
		}
		return rpcNetListener{listener}, nil
	case "pipe":
		return rpcListenPipe(address)
	}
	return nil, fmt.Errorf("no transport %q", transport)
}

type rpcNetListener struct {
	net.Listener
}

func (l rpcNetListener) accept() (io.ReadWriteCloser, error) { return l.Accept() }
func (l rpcNetListener) address() string                     { return l.Addr().String() }

func rpcDial(transport, address string) (io.ReadWriteCloser, error) {
	switch transport {
	case "tcp", "unix":
		return net.Dial(transport, address)
	case "pipe":
		return rpcDialPipe(address)
	}
	return nil, fmt.Errorf("no transport %q", transport)
}
