// Package flow is how a connection's frames leave and what paces them, the
// same at the server and at a client: a writer with no goroutine of its own,
// and credit, a sender's allowance and a receiver's window.
package flow

import (
	"context"
	"errors"
	"io"
	"sync"
	"sync/atomic"
)

// Writer writes a connection's frames from many goroutines without one of its
// own: the first sender to find nobody writing writes everything queued, what
// arrives meanwhile included, as UpdateGrouped's leader commits the writes
// behind it. A write a frame loses three to twenty-seven times at depth, and a
// goroutine that drains a queue costs a hand-off at one in flight,
// docs/reports/rpc-mechanics-2026-09-28.md.
type Writer struct {
	mu      sync.Mutex
	room    sync.Cond // queued bytes fell, or the writer failed
	queued  []byte
	spare   []byte
	writing bool
	w       io.Writer
	err     error
	bound   int  // bytes queued past which Send waits
	sealed  bool // a last frame was sent: nothing may follow it
}

// the largest queue kept for reuse once written
const keptQueue = 1 << 20

var ErrStopped = errors.New("the connection's writer stopped")

// NewWriter writes to w; bound is the bytes queued past which a Send waits
// for room, and twice it the queue past which a Post fails the writer.
func NewWriter(w io.Writer, bound int) *Writer {
	writer := &Writer{w: w, bound: bound}
	writer.room.L = &writer.mu
	return writer
}

// Send queues a copy of frame and, when nobody is writing, writes everything
// queued until nothing is. A frame that would take the queue past the bound
// waits for the writer to take the queue, unless the queue is empty.
func (w *Writer) Send(frame []byte) error {
	w.mu.Lock()
	for w.err == nil && w.writing && len(w.queued) > 0 && len(w.queued)+len(frame) > w.bound {
		w.room.Wait()
	}
	if err := w.refusal(); err != nil {
		w.mu.Unlock()
		return err
	}
	w.queued = append(w.queued, frame...)
	if w.writing {
		w.mu.Unlock()
		return nil
	}
	w.writing = true
	w.lead()
	err := w.err
	w.mu.Unlock()
	return err
}

// Post queues a copy of frame for a goroutine that must never wait, a
// connection's reader: when nobody is writing, a goroutine of its own writes
// it. A queue past twice the bound says the peer reads nothing, and fails the
// writer.
func (w *Writer) Post(frame []byte) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if err := w.refusal(); err != nil {
		return err
	}
	if len(w.queued)+len(frame) > 2*w.bound {
		w.stop(errors.New("the peer reads none of what is written to it"))
		return w.err
	}
	w.queued = append(w.queued, frame...)
	if !w.writing {
		w.writing = true
		go w.leadAlone()
	}
	return nil
}

// refusal is why a frame cannot be queued: the writer failed, or a last frame
// went before it
func (w *Writer) refusal() error {
	switch {
	case w.err != nil:
		return w.err
	case w.sealed:
		return ErrStopped
	}
	return nil
}

// SendLast writes frame as the connection's last, refusing every frame after
// it, and returns once it is written.
func (w *Writer) SendLast(frame []byte) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if err := w.refusal(); err != nil {
		return err
	}
	w.queued = append(w.queued, frame...)
	w.sealed = true
	if !w.writing {
		w.writing = true
		w.lead()
		return w.err
	}
	for w.err == nil && (w.writing || len(w.queued) > 0) {
		w.room.Wait()
	}
	return w.err
}

func (w *Writer) leadAlone() {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.lead()
}

// lead writes what is queued until nothing is, the lock held but while
// writing; the frames queued meanwhile go in the next write
func (w *Writer) lead() {
	for len(w.queued) > 0 && w.err == nil {
		batch := w.queued
		w.queued, w.spare = w.spare[:0], nil
		w.room.Broadcast()
		w.mu.Unlock()
		_, err := w.w.Write(batch)
		w.mu.Lock()
		if err != nil && w.err == nil {
			w.stop(err)
		}
		if cap(batch) <= keptQueue {
			w.spare = batch[:0]
		}
	}
	w.writing = false
	w.room.Broadcast()
}

// Flush waits until everything queued is written, or the writer failed.
func (w *Writer) Flush() error {
	w.mu.Lock()
	defer w.mu.Unlock()
	for w.err == nil && (w.writing || len(w.queued) > 0) {
		w.room.Wait()
	}
	return w.err
}

// Stop fails the writer with err: what is queued is dropped, and every Send
// and Post after it returns err.
func (w *Writer) Stop(err error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.stop(err)
}

func (w *Writer) stop(err error) {
	if w.err == nil {
		w.err = err
	}
	w.queued = nil
	w.room.Broadcast()
}

// Allowance is what a sender may still send on a stream: Take waits for the
// room a receiver's CREDIT grants.
type Allowance struct {
	mu      sync.Mutex
	bytes   int64
	ended   error
	granted chan struct{}
}

func NewAllowance(bytes int64) *Allowance {
	return &Allowance{bytes: bytes, granted: make(chan struct{}, 1)}
}

// Take waits until n bytes may be sent and takes them. Its one waiter is the
// stream's sender.
func (a *Allowance) Take(ctx context.Context, n int64) error {
	for {
		a.mu.Lock()
		switch {
		case a.ended != nil:
			err := a.ended
			a.mu.Unlock()
			return err
		case a.bytes >= n:
			a.bytes -= n
			a.mu.Unlock()
			return nil
		}
		a.mu.Unlock()
		select {
		case <-a.granted:
		case <-ctx.Done():
			return ctx.Err()
		}
	}
}

func (a *Allowance) Grant(n int64) {
	a.mu.Lock()
	a.bytes += n
	a.mu.Unlock()
	a.wake()
}

// End makes Take return err, for a stream or a connection that ended.
func (a *Allowance) End(err error) {
	a.mu.Lock()
	if a.ended == nil {
		a.ended = err
	}
	a.mu.Unlock()
	a.wake()
}

func (a *Allowance) wake() {
	select {
	case a.granted <- struct{}{}:
	default:
	}
}

// Credit is a receiver's side of a window: Receive counts what the sender
// sent and refuses what passes the window, and Consume says when to grant
// back what was let go, once half the window has gone, so that a grant is not
// a frame a frame. The reader receives while handlers consume.
type Credit struct {
	window      int64
	outstanding atomic.Int64 // sent and not granted back
	consumed    atomic.Int64 // let go and not granted back yet
}

func NewCredit(window int64) *Credit {
	return &Credit{window: window}
}

// Receive counts n bytes the sender sent, and says false, counting nothing,
// when they pass what it was granted.
func (c *Credit) Receive(n int64) bool {
	if c.outstanding.Add(n) > c.window {
		c.outstanding.Add(-n)
		return false
	}
	return true
}

// Consume counts n bytes let go and returns the bytes to grant back, zero
// until half the window has gone. The grant is taken from what is
// outstanding before it is returned, so that the sender's use of it is never
// counted against the bytes it replaces.
func (c *Credit) Consume(n int64) (grant int64) {
	total := c.consumed.Add(n)
	for total >= c.window/2 && total > 0 {
		if c.consumed.CompareAndSwap(total, 0) {
			c.outstanding.Add(-total)
			return total
		}
		total = c.consumed.Load()
	}
	return 0
}
