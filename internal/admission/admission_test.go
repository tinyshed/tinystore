package admission

import (
	"context"
	"errors"
	"testing"
	"time"
)

var errClosed = errors.New("closed")

// Close refuses new work at once and drains only when the work already in has left
func TestAClosedGateRefusesWorkAndDrainsWhenTheWorkLeaves(t *testing.T) {
	var gate Gate
	if err := gate.Enter(t.Context(), errClosed); err != nil {
		t.Fatal(err)
	}
	drained, first := gate.Close()
	if !first {
		t.Fatal("the first Close did not say so")
	}
	if err := gate.Enter(t.Context(), errClosed); !errors.Is(err, errClosed) {
		t.Fatalf("work entered a closed gate: %v", err)
	}
	select {
	case <-drained:
		t.Fatal("drained while work was in")
	default:
	}
	gate.Leave()
	<-drained
	if again, first := gate.Close(); first || again != drained {
		t.Fatal("a second Close closed the gate again")
	}
}

// a caller waiting for a slot leaves when its context ends, and a released
// slot lets the next one in
func TestSlotsHonourCancellation(t *testing.T) {
	slots := NewSlots(1)
	release, err := slots.Take(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	waiting, cancel := context.WithTimeout(t.Context(), 50*time.Millisecond)
	defer cancel()
	if _, err = slots.Take(waiting); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("a full slot let a caller in: %v", err)
	}
	release()
	release()
	next, err := slots.Take(t.Context())
	if err != nil {
		t.Fatalf("a released slot: %v", err)
	}
	next()
}
