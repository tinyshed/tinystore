package records

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/admission"
)

// appending, reading, sealing and following each hold their weight in the
// store's memory while they work, and give it back
func TestStoreMemoryBoundsAppendReadSealAndFollow(t *testing.T) {
	const capacity = 1 << 30
	s := openTestStore(t, t.TempDir(), Options{}, tinystore.Options{Memory: capacity})
	records := frontendRecords(maxSegmentRecords)
	work := map[string]func(context.Context) error{
		"append": func(ctx context.Context) error { return s.Append(ctx, records...) },
		"read": func(ctx context.Context) error {
			_, err := s.Read(ctx, Query{})
			return err
		},
		"seal": func(ctx context.Context) error {
			_, err := s.Maintain(ctx)
			return err
		},
		"follow": func(ctx context.Context) error {
			_, err := s.Follow(ctx, Cursor{}, 100)
			return err
		},
	}
	for _, name := range []string{"append", "read", "seal", "follow"} {
		reserved, err := s.runtime.Reserve(t.Context(), capacity)
		if err != nil {
			t.Fatal(err)
		}
		short, cancel := context.WithTimeout(t.Context(), 50*time.Millisecond)
		if err = work[name](short); !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("%s while the store's memory is taken: %v", name, err)
		}
		cancel()
		reserved.Release()
		if err = work[name](t.Context()); err != nil {
			t.Fatalf("%s once the memory is free: %v", name, err)
		}
		if usage := s.runtime.Memory(); usage.Used != 0 {
			t.Fatalf("%s kept %d bytes", name, usage.Used)
		}
	}
	if usage := s.runtime.Memory(); usage.Peak < segmentReservation {
		t.Fatalf("sealing a segment reserved at most %d bytes, want %d", usage.Peak, segmentReservation)
	}
}

// Reads and follows share the reader connections' slots and appends have their
// own, held through decoding and encoding. Work beyond them waits, and a caller
// that stops waiting leaves.
func TestReadsAndAppendsWaitForTheirSlots(t *testing.T) {
	s := openRecords(t)
	s.append(t, backendRecords(10)...)
	work := map[string]struct {
		slots admission.Slots
		run   func(context.Context) error
	}{
		"read": {s.reads, func(ctx context.Context) error {
			_, err := s.Read(ctx, Query{})
			return err
		}},
		"follow": {s.reads, func(ctx context.Context) error {
			_, err := s.Follow(ctx, Cursor{}, 10)
			return err
		}},
		"append": {s.appends, func(ctx context.Context) error { return s.Append(ctx, backendRecords(1)...) }},
	}
	for name, test := range work {
		for range cap(test.slots) {
			test.slots <- struct{}{}
		}
		short, cancel := context.WithTimeout(t.Context(), 50*time.Millisecond)
		if err := test.run(short); !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("%s while its slots are taken: %v", name, err)
		}
		cancel()
		for range cap(test.slots) {
			<-test.slots
		}
		if err := test.run(t.Context()); err != nil {
			t.Fatalf("%s once its slots are free: %v", name, err)
		}
	}
}

// a store whose memory cannot hold one segment in flight refuses to seal
// rather than seal past its budget
func TestASegmentLargerThanTheStoresMemoryIsRefused(t *testing.T) {
	s := openTestStore(t, t.TempDir(), Options{Budget: Budget{Bytes: 1 << 20}}, tinystore.Options{Memory: 16 << 20})
	s.append(t, frontendRecords(maxSegmentRecords)...)
	if _, err := s.Maintain(t.Context()); !errors.Is(err, tinystore.ErrLimit) {
		t.Fatalf("a 24 MiB segment in a 16 MiB store: %v", err)
	}
	if _, err := s.Read(t.Context(), Query{}); err != nil {
		t.Fatalf("a read within the memory: %v", err)
	}
}
