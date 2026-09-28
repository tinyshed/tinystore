package spike

import (
	"bufio"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/rand/v2"
	"os"
	"os/exec"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

// rpcClient multiplexes calls from many goroutines over one connection
type rpcClient struct {
	conn    io.ReadWriteCloser
	w       rpcWriter
	mu      sync.Mutex
	pending map[uint32]chan rpcFrame
	streams map[uint32]*rpcClientStream
	next    atomic.Uint32
	answers sync.Pool
	read    chan struct{}
	// creditAside grants a download's credit from a goroutine of its own, so
	// that a write never holds up the reading that consumes
	creditAside bool
}

// rpcClientStream is an upload's credit or a download's consumption
type rpcClientStream struct {
	allowance *rpcAllowance
	consumed  rpcConsumed
	received  int64
	finished  chan struct{}
}

func newRPCClient(conn io.ReadWriteCloser, writer string) (*rpcClient, error) {
	w, err := newRPCWriter(writer, conn)
	if err != nil {
		return nil, err
	}
	c := &rpcClient{
		conn: conn, w: w, read: make(chan struct{}),
		pending: map[uint32]chan rpcFrame{}, streams: map[uint32]*rpcClientStream{},
	}
	go c.receive()
	return c, nil
}

func (c *rpcClient) close() {
	_ = c.conn.Close()
	<-c.read
	c.w.close()
}

func (c *rpcClient) receive() {
	defer close(c.read)
	reader := newRPCReader(c.conn, nil)
	for {
		f, err := reader.next()
		if err != nil {
			return
		}
		switch f.kind {
		case rpcResponse:
			c.mu.Lock()
			answer := c.pending[f.stream]
			delete(c.pending, f.stream)
			c.mu.Unlock()
			if answer != nil {
				answer <- f
			}
		case rpcCredit:
			if s := c.stream(f.stream); s != nil && s.allowance != nil {
				s.allowance.grant(int64(binary.LittleEndian.Uint32(f.body)))
			}
		case rpcData:
			c.consume(f)
		}
	}
}

func (c *rpcClient) stream(stream uint32) *rpcClientStream {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.streams[stream]
}

// consume counts a download's bytes and grants credit back as they go
func (c *rpcClient) consume(f rpcFrame) {
	s := c.stream(f.stream)
	if s == nil || s.finished == nil {
		return
	}
	s.received += int64(len(f.body))
	if f.flags&rpcEnd != 0 {
		c.mu.Lock()
		delete(c.streams, f.stream)
		c.mu.Unlock()
		close(s.finished)
		return
	}
	if grant := s.consumed.add(int64(len(f.body))); grant > 0 {
		credit := rpcCreditFrame(f.stream, grant)
		if c.creditAside {
			go func() { _ = c.w.send(credit) }()
			return
		}
		_ = c.w.send(credit)
	}
}

func (c *rpcClient) open() (uint32, chan rpcFrame) {
	stream := c.next.Add(1)
	answer, _ := c.answers.Get().(chan rpcFrame)
	if answer == nil {
		answer = make(chan rpcFrame, 1)
	}
	c.mu.Lock()
	c.pending[stream] = answer
	c.mu.Unlock()
	return stream, answer
}

func (c *rpcClient) call(method uint16, body []byte) ([]byte, error) {
	stream, answer := c.open()
	if err := c.w.send(rpcAppendFrame(nil, rpcRequest, rpcEnd, method, stream, body)); err != nil {
		return nil, err
	}
	select {
	case f := <-answer:
		c.answers.Put(answer)
		return f.body, nil
	case <-c.read:
		return nil, errors.New("the connection ended")
	}
}

// upload sends total bytes as a stream within the window the server grants
func (c *rpcClient) upload(total, window int64) error {
	stream, answer := c.open()
	s := &rpcClientStream{allowance: newRPCAllowance(window)}
	c.mu.Lock()
	c.streams[stream] = s
	c.mu.Unlock()
	if err := c.w.send(rpcAppendFrame(nil, rpcRequest, 0, rpcUpload, stream,
		binary.LittleEndian.AppendUint32(nil, uint32(window)))); err != nil {
		return err
	}
	chunk := make([]byte, rpcChunk)
	var frame []byte
	for sent := int64(0); sent < total; {
		n := min(int64(rpcChunk), total-sent)
		if err := s.allowance.take(n); err != nil {
			return err
		}
		sent += n
		flags := byte(0)
		if sent == total {
			flags = rpcEnd
		}
		if !rpcWriterCopies(c.w) {
			frame = nil
		}
		frame = rpcAppendFrame(frame[:0], rpcData, flags, 0, stream, chunk[:n])
		if err := c.w.send(frame); err != nil {
			return err
		}
	}
	f := <-answer
	c.mu.Lock()
	delete(c.streams, stream)
	c.mu.Unlock()
	if got := int64(binary.LittleEndian.Uint64(f.body)); got != total {
		return fmt.Errorf("the server consumed %d bytes of %d", got, total)
	}
	return nil
}

// download asks for total bytes and grants the server window of them at a time
func (c *rpcClient) download(total, window int64) error {
	stream := c.next.Add(1)
	s := &rpcClientStream{consumed: rpcConsumed{window: window}, finished: make(chan struct{})}
	c.mu.Lock()
	c.streams[stream] = s
	c.mu.Unlock()
	body := binary.LittleEndian.AppendUint64(nil, uint64(total))
	body = binary.LittleEndian.AppendUint32(body, uint32(window))
	if err := c.w.send(rpcAppendFrame(nil, rpcRequest, rpcEnd, rpcDownload, stream, body)); err != nil {
		return err
	}
	select {
	case <-s.finished:
	case <-c.read:
		return errors.New("the connection ended")
	}
	if s.received != total {
		return fmt.Errorf("received %d bytes of %d", s.received, total)
	}
	return nil
}

func (c *rpcClient) stats() (rpcStatsAnswer, error) {
	body, err := c.call(rpcStats, nil)
	if err != nil {
		return rpcStatsAnswer{}, err
	}
	var stats rpcStatsAnswer
	return stats, json.Unmarshal(body, &stats)
}

// rpcSidecar is a sidecar process and, but for stdio, the address it listens on
type rpcSidecar struct {
	cmd      *exec.Cmd
	lifeline io.WriteCloser
	stdout   io.ReadCloser
	address  string
}

// rpcSidecarCommand is the test binary run as a sidecar with config
func rpcSidecarCommand(config rpcSidecarConfig) ([]string, string) {
	text, _ := json.Marshal(config)
	return []string{os.Args[0]}, string(text)
}

func startRPCSidecar(t *testing.T, config rpcSidecarConfig) *rpcSidecar {
	t.Helper()
	argv, text := rpcSidecarCommand(config)
	cmd := exec.Command(argv[0])
	cmd.Env = append(os.Environ(), rpcSidecarVariable+"="+text)
	cmd.Stderr = os.Stderr
	lifeline, err := cmd.StdinPipe()
	if err != nil {
		t.Fatal(err)
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	s := &rpcSidecar{cmd: cmd, lifeline: lifeline, stdout: stdout}
	t.Cleanup(s.stop)
	if config.Transport != "stdio" {
		s.address = readRPCReady(t, stdout)
	}
	return s
}

func readRPCReady(t *testing.T, stdout io.Reader) string {
	t.Helper()
	line, err := bufio.NewReader(stdout).ReadString('\n')
	ready, address, found := strings.Cut(strings.TrimSpace(line), " ")
	if err != nil || !found || ready != "ready" {
		t.Fatalf("the sidecar did not start: %q %v", line, err)
	}
	return address
}

func (s *rpcSidecar) stop() {
	_ = s.lifeline.Close()
	done := make(chan struct{})
	go func() {
		_ = s.cmd.Wait()
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(30 * time.Second):
		_ = s.cmd.Process.Kill()
		<-done
	}
}

// connect is a client of the sidecar over its transport; over stdio the
// sidecar's own pipes are the connection
func (s *rpcSidecar) connect(t *testing.T, config rpcSidecarConfig) *rpcClient {
	t.Helper()
	var conn io.ReadWriteCloser
	if config.Transport == "stdio" {
		conn = rpcPipes{r: s.stdout, w: s.lifeline}
	} else {
		dialed, err := rpcDial(config.Transport, s.address)
		if err != nil {
			t.Fatal(err)
		}
		conn = dialed
	}
	c, err := newRPCClient(conn, config.Writer)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := c.call(rpcEcho, nil); err != nil { // the sidecar has filled its bucket
		t.Fatal(err)
	}
	return c
}

// rpcPipes is a child's stdout and stdin as one connection
type rpcPipes struct {
	r io.ReadCloser
	w io.WriteCloser
}

func (p rpcPipes) Read(b []byte) (int, error)  { return p.r.Read(b) }
func (p rpcPipes) Write(b []byte) (int, error) { return p.w.Write(b) }
func (p rpcPipes) Close() error                { return p.w.Close() }

// rpcRun runs op from depth goroutines for d, and counts what they finished
func rpcRun(depth int, d time.Duration, op func(r *rand.Rand) error) (float64, error) {
	var done atomic.Bool
	var count atomic.Int64
	var failed atomic.Pointer[error]
	var running sync.WaitGroup
	began := time.Now()
	for w := range depth {
		running.Go(func() {
			r := rand.New(rand.NewPCG(uint64(w), 7))
			for !done.Load() {
				if err := op(r); err != nil {
					failed.Store(&err)
					return
				}
				count.Add(1)
			}
		})
	}
	time.Sleep(d)
	done.Store(true)
	running.Wait()
	if err := failed.Load(); err != nil {
		return 0, *err
	}
	return float64(count.Load()) / time.Since(began).Seconds(), nil
}
