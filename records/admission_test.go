package records

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
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
		release, err := s.runtime.Reserve(t.Context(), capacity)
		if err != nil {
			t.Fatal(err)
		}
		short, cancel := context.WithTimeout(t.Context(), 50*time.Millisecond)
		if err = work[name](short); !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("%s while the store's memory is taken: %v", name, err)
		}
		cancel()
		release()
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
