package admission

import (
	"context"
	"errors"
	"sync"
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

// work entering and leaving from many goroutines while the gate closes is
// either refused or counted, and the gate drains once all it counted left
func TestAGateClosingBesideWorkDrainsOnceItsWorkHasLeft(t *testing.T) {
	for range 100 {
		var gate Gate
		var inside sync.WaitGroup
		start := make(chan struct{})
		for range 16 {
			inside.Go(func() {
				<-start
				for range 100 {
					if gate.Enter(t.Context(), errClosed) == nil {
						gate.Leave()
					}
				}
			})
		}
		if err := gate.Enter(t.Context(), errClosed); err != nil {
			t.Fatal(err)
		}
		close(start)
		drained, _ := gate.Close()
		select {
		case <-drained:
			t.Fatal("drained while work was in")
		default:
		}
		gate.Leave()
		inside.Wait()
		<-drained
		if err := gate.Enter(t.Context(), errClosed); !errors.Is(err, errClosed) {
			t.Fatalf("work entered a drained gate: %v", err)
		}
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

func TestSlotsCanBeTakenWithoutWaiting(t *testing.T) {
	slots := NewSlots(1)
	release, taken := slots.TryTake()
	if !taken {
		t.Fatal("a free slot was refused")
	}
	if _, taken = slots.TryTake(); taken {
		t.Fatal("a full slot admitted more work")
	}
	release()
	release()
	next, taken := slots.TryTake()
	if !taken {
		t.Fatal("a released slot was not free")
	}
	next()
}
