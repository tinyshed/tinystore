package flow

import (
	"bytes"
	"context"
	"errors"
	"sync"
	"testing"
	"time"
)

// heldWriter holds its first write until released, and records every write
type heldWriter struct {
	mu       sync.Mutex
	writes   [][]byte
	entered  chan struct{}
	released chan struct{}
}

func newHeldWriter() *heldWriter {
	return &heldWriter{entered: make(chan struct{}, 1), released: make(chan struct{})}
}

func (h *heldWriter) Write(p []byte) (int, error) {
	h.mu.Lock()
	first := len(h.writes) == 0
	h.writes = append(h.writes, bytes.Clone(p))
	h.mu.Unlock()
	if first {
		h.entered <- struct{}{}
		<-h.released
	}
	return len(p), nil
}

func TestQueuedAnswersShareAWrite(t *testing.T) {
	out := newHeldWriter()
	w := NewWriter(out, 1<<20)
	go func() { _ = w.Send([]byte("first")) }()
	<-out.entered

	var queued sync.WaitGroup
	for range 100 {
		queued.Go(func() {
			if err := w.Send([]byte("answer")); err != nil {
				t.Error(err)
			}
		})
	}
	queued.Wait() // each Send returned without writing: the leader holds the write
	close(out.released)
	if err := w.Flush(); err != nil {
		t.Fatal(err)
	}

	out.mu.Lock()
	defer out.mu.Unlock()
	if len(out.writes) != 2 || len(out.writes[1]) != 100*len("answer") {
		t.Fatalf("%d writes, the second of %d bytes; want the hundred answers in one", len(out.writes),
			len(out.writes[len(out.writes)-1]))
	}
}

func TestASendPastTheBoundWaitsForTheLeaderToTakeTheQueue(t *testing.T) {
	out := newHeldWriter()
	w := NewWriter(out, 10)
	go func() { _ = w.Send([]byte("first")) }()
	<-out.entered
	if err := w.Send([]byte("0123456789")); err != nil { // an empty queue takes it whole
		t.Fatal(err)
	}

	sent := make(chan struct{})
	go func() {
		_ = w.Send([]byte("x"))
		close(sent)
	}()
	select {
	case <-sent:
		t.Fatal("a send past the bound did not wait")
	case <-time.After(50 * time.Millisecond):
	}
	close(out.released)
	<-sent
	if err := w.Flush(); err != nil {
		t.Fatal(err)
	}
}

func TestAPostNeverWaitsAndFailsAPeerThatReadsNothing(t *testing.T) {
	out := newHeldWriter()
	w := NewWriter(out, 10)
	if err := w.Post([]byte("first")); err != nil {
		t.Fatal(err)
	}
	<-out.entered
	for range 5 {
		if err := w.Post([]byte("pong")); err != nil {
			t.Fatal(err)
		}
	}
	if err := w.Post([]byte("pong")); err == nil {
		t.Fatal("a queue past twice the bound was taken")
	}
	if err := w.Send([]byte("x")); err == nil {
		t.Fatal("a stopped writer took a frame")
	}
	close(out.released)
}

func TestAnAllowanceWaitsForItsGrants(t *testing.T) {
	a := NewAllowance(10)
	if err := a.Take(t.Context(), 10); err != nil {
		t.Fatal(err)
	}
	taken := make(chan error, 1)
	go func() { taken <- a.Take(t.Context(), 5) }()
	select {
	case err := <-taken:
		t.Fatalf("took past the allowance: %v", err)
	case <-time.After(20 * time.Millisecond):
	}
	a.Grant(3)
	a.Grant(2)
	if err := <-taken; err != nil {
		t.Fatal(err)
	}

	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if err := a.Take(ctx, 1); !errors.Is(err, context.Canceled) {
		t.Fatalf("a cancelled take: %v", err)
	}
	ended := errors.New("ended")
	a.End(ended)
	if err := a.Take(t.Context(), 0); !errors.Is(err, ended) {
		t.Fatalf("an ended allowance: %v", err)
	}
}

func TestCreditIsGrantedBackOnceHalfTheWindowHasGone(t *testing.T) {
	c := NewCredit(100)
	if !c.Receive(100) || c.Receive(1) {
		t.Fatal("the window's edge")
	}
	if grant := c.Consume(49); grant != 0 {
		t.Fatalf("granted %d before half the window had gone", grant)
	}
	if grant := c.Consume(1); grant != 50 {
		t.Fatalf("granted %d at half the window", grant)
	}
	if !c.Receive(50) || c.Receive(1) {
		t.Fatal("the window after its grant")
	}
}
