package spike

import (
	"bufio"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"sync"
)

// frames as docs/wire.md lays them out: a body's length, its kind, flags,
// a method and a stream, little-endian, then the body
const rpcHeaderSize = 12

const (
	rpcRequest  = 3
	rpcResponse = 4
	rpcData     = 5
	rpcCredit   = 7
)

const rpcEnd = 1

// the probe's operations; the real ones are wire.md's, fixed with each slice
const (
	rpcEcho uint16 = iota + 1
	rpcGet
	rpcSet
	rpcUpload
	rpcDownload
	rpcStats
	rpcProfileStart
	rpcProfileStop
)

// the largest body a probe frame carries: a 64 KiB chunk of a stream and more
const rpcMaxBody = 1 << 20

// a stream's bytes travel in frames of this size, as wire.md asks of senders
const rpcChunk = 64 << 10

type rpcFrame struct {
	kind, flags byte
	method      uint16
	stream      uint32
	body        []byte
	release     func()
}

func rpcAppendFrame(dst []byte, kind, flags byte, method uint16, stream uint32, body []byte) []byte {
	dst = binary.LittleEndian.AppendUint32(dst, uint32(len(body)))
	dst = append(dst, kind, flags)
	dst = binary.LittleEndian.AppendUint16(dst, method)
	dst = binary.LittleEndian.AppendUint32(dst, stream)
	return append(dst, body...)
}

// rpcBodies lends small bodies from a pool, so that a call allocates none of
// its own; a larger body is allocated
type rpcBodies struct {
	pool sync.Pool
}

const rpcPooledBody = 4 << 10

func (b *rpcBodies) take(n int) ([]byte, func()) {
	if n > rpcPooledBody {
		return make([]byte, n), func() {}
	}
	held, _ := b.pool.Get().(*[]byte)
	if held == nil {
		fresh := make([]byte, rpcPooledBody)
		held = &fresh
	}
	return (*held)[:n], func() { b.pool.Put(held) }
}

type rpcReader struct {
	r      *bufio.Reader
	bodies *rpcBodies // nil allocates every body
	header [rpcHeaderSize]byte
}

func newRPCReader(r io.Reader, bodies *rpcBodies) *rpcReader {
	return &rpcReader{r: bufio.NewReaderSize(r, 64<<10), bodies: bodies}
}

func (r *rpcReader) next() (rpcFrame, error) {
	if _, err := io.ReadFull(r.r, r.header[:]); err != nil {
		return rpcFrame{}, err
	}
	length := binary.LittleEndian.Uint32(r.header[0:])
	if length > rpcMaxBody {
		return rpcFrame{}, fmt.Errorf("a body of %d bytes, %d agreed", length, rpcMaxBody)
	}
	f := rpcFrame{
		kind: r.header[4], flags: r.header[5],
		method: binary.LittleEndian.Uint16(r.header[6:]),
		stream: binary.LittleEndian.Uint32(r.header[8:]),
	}
	if r.bodies != nil {
		f.body, f.release = r.bodies.take(int(length))
	} else {
		f.body, f.release = make([]byte, length), func() {}
	}
	if _, err := io.ReadFull(r.r, f.body); err != nil {
		f.release()
		return rpcFrame{}, err
	}
	return f, nil
}

// rpcWriter sends whole frames from many goroutines; the ways the round
// compares differ in who makes the write and how many frames it carries
type rpcWriter interface {
	// send may keep frame until it is written: a caller hands over what it sends
	send(frame []byte) error
	close()
}

func newRPCWriter(how string, w io.Writer) (rpcWriter, error) {
	switch how {
	case "naive":
		return &rpcNaiveWriter{w: w}, nil
	case "goroutine":
		return newRPCGoroutineWriter(w), nil
	case "leader":
		return &rpcLeaderWriter{w: w}, nil
	case "leader-256k":
		return &rpcLeaderWriter{w: w, most: 256 << 10}, nil
	}
	return nil, fmt.Errorf("no writer %q", how)
}

// rpcNaiveWriter writes each frame with a write of its own
type rpcNaiveWriter struct {
	mu  sync.Mutex
	w   io.Writer
	err error
}

func (n *rpcNaiveWriter) send(frame []byte) error {
	n.mu.Lock()
	defer n.mu.Unlock()
	if n.err == nil {
		_, n.err = n.w.Write(frame)
	}
	return n.err
}

func (n *rpcNaiveWriter) close() {}

// rpcGoroutineWriter is the probe's: a goroutine of its own drains what is
// queued into one buffered write
type rpcGoroutineWriter struct {
	out  chan []byte
	done chan struct{}
	w    io.Writer
}

func newRPCGoroutineWriter(w io.Writer) *rpcGoroutineWriter {
	g := &rpcGoroutineWriter{out: make(chan []byte, 4096), done: make(chan struct{}), w: w}
	go g.loop()
	return g
}

func (g *rpcGoroutineWriter) send(frame []byte) error {
	g.out <- frame
	return nil
}

func (g *rpcGoroutineWriter) close() {
	close(g.out)
	<-g.done
}

func (g *rpcGoroutineWriter) loop() {
	defer close(g.done)
	buffered := bufio.NewWriterSize(g.w, 64<<10)
	failed := false
	for frame := range g.out {
		if failed {
			continue
		}
		_, _ = buffered.Write(frame)
	drain:
		for {
			select {
			case more, ok := <-g.out:
				if !ok {
					break drain
				}
				_, _ = buffered.Write(more)
			default:
				break drain
			}
		}
		failed = buffered.Flush() != nil
	}
}

// rpcLeaderWriter has no goroutine: the first sender to find nobody writing
// writes everything queued, what arrives meanwhile included, as
// UpdateGrouped's leader commits for the writes behind it
type rpcLeaderWriter struct {
	mu      sync.Mutex
	queued  []byte
	spare   []byte
	writing bool
	w       io.Writer
	err     error
	most    int // the most one write carries; zero writes all that is queued
}

func (l *rpcLeaderWriter) send(frame []byte) error {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.err != nil {
		return l.err
	}
	l.queued = append(l.queued, frame...)
	if l.writing {
		return nil
	}
	l.writing = true
	for len(l.queued) > 0 && l.err == nil {
		batch := l.queued
		l.queued, l.spare = l.spare[:0], nil
		l.mu.Unlock()
		err := l.write(batch)
		l.mu.Lock()
		l.spare, l.err = batch[:0], err
	}
	l.writing = false
	return l.err
}

func (l *rpcLeaderWriter) write(batch []byte) error {
	for len(batch) > 0 {
		n := len(batch)
		if l.most > 0 {
			n = min(n, l.most)
		}
		if _, err := l.w.Write(batch[:n]); err != nil {
			return err
		}
		batch = batch[n:]
	}
	return nil
}

func (l *rpcLeaderWriter) close() {}

// rpcAllowance is what a sender may still send on a stream
type rpcAllowance struct {
	mu      sync.Mutex
	granted sync.Cond
	bytes   int64
	closed  bool
}

func newRPCAllowance(bytes int64) *rpcAllowance {
	c := &rpcAllowance{bytes: bytes}
	c.granted.L = &c.mu
	return c
}

// take waits until n bytes may be sent, and takes them
func (c *rpcAllowance) take(n int64) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	for c.bytes < n && !c.closed {
		c.granted.Wait()
	}
	if c.closed {
		return errors.New("the stream ended")
	}
	c.bytes -= n
	return nil
}

func (c *rpcAllowance) grant(n int64) {
	c.mu.Lock()
	c.bytes += n
	c.mu.Unlock()
	c.granted.Signal()
}

func (c *rpcAllowance) end() {
	c.mu.Lock()
	c.closed = true
	c.mu.Unlock()
	c.granted.Broadcast()
}

// rpcConsumed counts what a receiver consumed, and says when half its window
// has gone since it last granted: a credit a frame would double the frames
type rpcConsumed struct {
	window, since int64
}

func (c *rpcConsumed) add(n int64) (grant int64) {
	c.since += n
	if c.since < c.window/2 {
		return 0
	}
	grant, c.since = c.since, 0
	return grant
}

func rpcCreditFrame(stream uint32, n int64) []byte {
	return rpcAppendFrame(nil, rpcCredit, 0, 0, stream, binary.LittleEndian.AppendUint32(nil, uint32(n)))
}
