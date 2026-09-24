package tinystore

import (
	"context"
	"errors"
	"testing"
	"time"
)

func TestMemoryGrantsInArrivalOrder(t *testing.T) {
	budget := &memory{capacity: 10}
	if err := budget.acquire(t.Context(), 6); err != nil {
		t.Fatal(err)
	}
	large, small := make(chan error, 1), make(chan error, 1)
	go func() { large <- budget.acquire(t.Context(), 8) }()
	waitForWaiters(t, budget, 1)
	go func() { small <- budget.acquire(t.Context(), 3) }()
	waitForWaiters(t, budget, 2)

	budget.release(6)
	if err := <-large; err != nil {
		t.Fatal(err)
	}
	select {
	case <-small:
		t.Fatal("the small reservation overtook the queue")
	case <-time.After(20 * time.Millisecond):
	}
	budget.release(8)
	if err := <-small; err != nil {
		t.Fatal(err)
	}
	if usage := budget.usage(); usage.Used != 3 || usage.Peak != 8 {
		t.Fatalf("usage %+v", usage)
	}
}

func TestMemoryCancelledWaiterLetsTheNextOneIn(t *testing.T) {
	budget := &memory{capacity: 10}
	if err := budget.acquire(t.Context(), 6); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(t.Context())
	large, small := make(chan error, 1), make(chan error, 1)
	go func() { large <- budget.acquire(ctx, 8) }()
	waitForWaiters(t, budget, 1)
	go func() { small <- budget.acquire(t.Context(), 3) }()
	waitForWaiters(t, budget, 2)

	cancel()
	if err := <-large; !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled waiter: %v", err)
	}
	if err := <-small; err != nil {
		t.Fatal(err)
	}
	if usage := budget.usage(); usage.Used != 9 {
		t.Fatalf("usage %+v", usage)
	}
}

func TestAReservationLargerThanTheStoreIsRefused(t *testing.T) {
	store := openTestStore(t, t.TempDir(), Options{Memory: 100})
	if _, err := store.Reserve(t.Context(), 101); !errors.Is(err, ErrLimit) {
		t.Fatalf("101 bytes of 100: %v", err)
	}
	release, err := store.Reserve(t.Context(), 100)
	if err != nil {
		t.Fatal(err)
	}
	release()
	release()
	if usage := store.Memory(); usage != (MemoryUsage{Used: 0, Peak: 100, Capacity: 100}) {
		t.Fatalf("usage after a release called twice: %+v", usage)
	}
}

func TestAStoreWithoutMemoryGrantsEveryReservation(t *testing.T) {
	store := openTestStore(t, t.TempDir(), Options{})
	release, err := store.Reserve(t.Context(), 1<<62)
	if err != nil {
		t.Fatal(err)
	}
	release()
	if usage := store.Memory(); usage != (MemoryUsage{}) {
		t.Fatalf("usage %+v", usage)
	}
	if _, err = Open(t.Context(), t.TempDir(), Options{Memory: -1}); !errors.Is(err, ErrInvalid) {
		t.Fatalf("negative memory: %v", err)
	}
}

func waitForWaiters(t *testing.T, budget *memory, count int) {
	t.Helper()
	for deadline := time.Now().Add(5 * time.Second); time.Now().Before(deadline); time.Sleep(time.Millisecond) {
		budget.mu.Lock()
		waiting := len(budget.waiting)
		budget.mu.Unlock()
		if waiting == count {
			return
		}
	}
	t.Fatalf("never saw %d waiters", count)
}
