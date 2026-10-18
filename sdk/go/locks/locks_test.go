package locks_test

import (
	"context"
	"errors"
	"testing"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/ways"
	"tinyshed.org/tinystore/locks"
)

func is(t *testing.T, err error, kind error) {
	t.Helper()
	if !errors.Is(err, kind) {
		t.Fatalf("got %v, want %v", err, kind)
	}
}

func nothing(context.Context, *locks.Lease) error { return nil }

// holding holds key on a goroutine of its own until let go is closed, and
// says its fence once it holds it.
func holding(t *testing.T, hold func(context.Context, any, func(context.Context, *locks.Lease) error, ...locks.HoldOption) error, key string) (chan locks.Fence, chan struct{}, chan error) {
	t.Helper()
	fences, letGo, ended := make(chan locks.Fence, 1), make(chan struct{}), make(chan error, 1)
	go func() {
		ended <- hold(context.Background(), key, func(ctx context.Context, lease *locks.Lease) error {
			fences <- lease.Fence
			select {
			case <-letGo:
			case <-ctx.Done():
			}
			return nil
		})
	}()
	return fences, letGo, ended
}

func TestAKeyIsHeldByOneHolderAtATimeTheNextInLineGivenItWhenItIsLetGo(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		invoices := ways.Must(locks.OpenLock(store, "invoices"))
		fences, letGo, ended := holding(t, invoices.Hold, "42")
		before := <-fences
		if took := ways.Must(invoices.TryHold(ctx, "42", nothing)); took {
			t.Fatal("a held key was taken")
		}
		if took := ways.Must(invoices.TryHold(ctx, 43, nothing)); !took {
			t.Fatal("a free key was not taken")
		}
		next := make(chan locks.Fence, 1)
		go func() {
			ways.Must(0, invoices.Hold(ctx, "42", func(_ context.Context, lease *locks.Lease) error {
				next <- lease.Fence
				return nil
			}))
		}()
		time.Sleep(50 * time.Millisecond)
		close(letGo)
		ways.Must(0, <-ended)
		if after := <-next; !before.Less(after) || after.String() == "0" {
			t.Fatalf("fences %s then %s", before, after)
		}
	})
}

func TestAKeyStillHeldPastTheWaitIsInUseAndAnErrorLetsTheKeyGo(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		reports := ways.Must(locks.OpenLock(store, "reports"))
		fences, letGo, ended := holding(t, reports.Hold, "sales")
		<-fences
		err := reports.Hold(ctx, "sales", nothing, locks.Wait(200*time.Millisecond))
		is(t, err, tinystore.ErrInUse)
		close(letGo)
		ways.Must(0, <-ended)
		failed := errors.New("the holder failed")
		if err := reports.Hold(ctx, "sales", func(context.Context, *locks.Lease) error { return failed }); !errors.Is(err, failed) {
			t.Fatalf("hold %v", err)
		}
		if !ways.Must(reports.TryHold(ctx, "sales", nothing)) {
			t.Fatal("an error kept the key")
		}
	})
}

func TestAHoldInsideItsOwnHoldOfTheKeyIsInvalid(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		nested := ways.Must(locks.OpenLock(store, "nested"))
		err := nested.Hold(ctx, "42", func(ctx context.Context, _ *locks.Lease) error {
			return nested.Hold(ctx, "42", nothing)
		})
		is(t, err, tinystore.ErrInvalid)
		ways.Must(0, nested.Hold(ctx, "42", func(ctx context.Context, _ *locks.Lease) error {
			return nested.Hold(ctx, "43", func(_ context.Context, inner *locks.Lease) error {
				if !(locks.Fence{}).Less(inner.Fence) {
					t.Errorf("an inner fence %s", inner.Fence)
				}
				return nil
			})
		}))
	})
}

func TestALeaseRenewedByThisProgramOutlivesItsTTL(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		jobs := ways.Must(locks.OpenLock(store, "long", locks.TTL(time.Second)))
		fences, letGo, ended := holding(t, jobs.Hold, "nightly")
		<-fences
		time.Sleep(1600 * time.Millisecond)
		if ways.Must(jobs.TryHold(ctx, "nightly", nothing)) {
			t.Fatal("a renewed lease ran out")
		}
		close(letGo)
		ways.Must(0, <-ended)
	})
}

func TestMaxHoldRevokesALeaseGivesTheKeyOnAndEndsTheHoldLost(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		exports := ways.Must(locks.OpenLock(store, "exports", locks.MaxHold(300*time.Millisecond)))
		started := make(chan struct{})
		ended := make(chan error, 1)
		go func() {
			ended <- exports.Hold(ctx, "daily", func(ctx context.Context, _ *locks.Lease) error {
				close(started)
				select {
				case <-ctx.Done():
					return context.Cause(ctx)
				case <-time.After(10 * time.Second):
					return errors.New("past the sleep")
				}
			})
		}()
		<-started
		ways.Must(0, exports.Hold(ctx, "daily", nothing, locks.Wait(5*time.Second)))
		is(t, <-ended, tinystore.ErrLost)
		// a lease lost after the function's last wait still ends the hold lost
		late := ways.Must(locks.OpenLock(store, "late", locks.MaxHold(100*time.Millisecond)))
		err := late.Hold(ctx, "daily", func(context.Context, *locks.Lease) error {
			time.Sleep(300 * time.Millisecond)
			return nil
		})
		is(t, err, tinystore.ErrLost)
	})
}

func TestASemaphoreAdmitsItsLimitAKeyItsPermitWithoutAFence(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		uploads := ways.Must(locks.OpenSemaphore(store, "uploads", locks.Limit(2)))
		firsts, letGoFirst, firstEnded := holding(t, uploads.Hold, "ann")
		seconds, letGoSecond, secondEnded := holding(t, uploads.Hold, "ann")
		if (<-firsts != locks.Fence{}) || (<-seconds != locks.Fence{}) {
			t.Fatal("a permit has a fence")
		}
		if ways.Must(uploads.TryHold(ctx, "ann", nothing)) {
			t.Fatal("a third took a key of two permits")
		}
		if !ways.Must(uploads.TryHold(ctx, "bob", nothing)) {
			t.Fatal("another key was not free")
		}
		close(letGoFirst)
		close(letGoSecond)
		ways.Must(0, <-firstEnded)
		ways.Must(0, <-secondEnded)
	})
}

func TestNamesAndLimitsAreCheckedBeforeAnythingIsSent(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		_, err := locks.OpenLock(store, "Invoices")
		is(t, err, tinystore.ErrInvalid)
		_, err = locks.OpenSemaphore(store, "uploads", locks.Limit(0))
		is(t, err, tinystore.ErrInvalid)
	})
}
