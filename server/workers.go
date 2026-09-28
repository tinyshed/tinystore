package server

import (
	"errors"
	"fmt"
	"runtime/debug"
	"sync"
	"sync/atomic"

	"github.com/tinyshed/tinystore/server/wire"
)

// workers run a connection's calls on goroutines that live as long as it
// does, as many as its streams in flight at most. A goroutine started for
// each call grows its stack on the way into SQLite every time, a quarter of a
// sidecar's time in the round, and too few workers starve a group commit of
// the writes that would join it, docs/reports/rpc-mechanics-2026-09-28.md.
type workers struct {
	calls   chan *call
	busy    atomic.Int64 // calls handed over and not yet finished
	started int64        // workers started; only the reader starts them
	running sync.WaitGroup
}

// run hands a call to a worker, starting one when every worker is busy, so
// that no call waits for another to finish
func (w *workers) run(s *session, c *call) {
	if w.busy.Add(1) > w.started {
		w.started++
		w.running.Go(func() { s.work(w.calls) })
	}
	w.calls <- c
}

// stop lets the workers finish the calls handed over, and waits for them
func (w *workers) stop() {
	close(w.calls)
	w.running.Wait()
}

func (s *session) work(calls <-chan *call) {
	frame := make([]byte, 0, 4<<10)
	for c := range calls {
		c.frame = frame
		s.handle(c)
		frame = c.frame
		s.workers.busy.Add(-1)
	}
}

type handler func(c *call) error

var errNoAnswer = errors.New("server: a handler returned without answering its stream")

// handle runs a call's handler and ends its stream once, whatever the handler
// did: an error or a panic becomes the stream's error
func (s *session) handle(c *call) {
	var err error
	defer func() {
		if recovered := recover(); recovered != nil {
			s.log.Error("a handler panicked", "method", fmt.Sprintf("%#04x", uint16(c.method)), "panic", recovered,
				"stack", string(debug.Stack()))
			err = fmt.Errorf("%w: a handler panicked", errInternal)
		}
		s.settle(c, err)
	}()
	run := s.server.methods[c.method]
	if run == nil {
		err = &wire.Error{Code: wire.CodeUnimplemented, Message: fmt.Sprintf("method %#04x", uint16(c.method))}
		return
	}
	err = run(c)
}

// settle sends the final frame a handler did not, and gives back the request
// and whatever the client uploaded that the handler left
func (s *session) settle(c *call, err error) {
	if !c.ended && err == nil {
		err = errNoAnswer
	}
	if !c.ended {
		err = c.fail(err)
	}
	// a final frame that could not be written ended the connection already
	if err != nil && s.ctx.Err() == nil {
		s.log.Debug("a handler failed after its stream ended", "method", fmt.Sprintf("%#04x", uint16(c.method)),
			"error", err)
	}
	c.cancel(nil)

	letGo := int64(len(c.request))
	giveBody(c.request)
	c.request = nil
	if c.upload != nil {
		for _, body := range c.upload.drain() {
			letGo += int64(len(body))
			giveBody(body)
		}
	}
	s.letGo(letGo)
}

var errInternal = errors.New("internal error")
