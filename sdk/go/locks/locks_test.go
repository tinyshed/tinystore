package locks_test

import (
	"context"
	"errors"
	"slices"
	"strings"
	"sync"
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

// contract: LOCK-08
func TestAHoldingSaysHowLongItWaitedAtLeastAsLongAsItStoodInLine(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		moodle := ways.Must(locks.OpenSemaphore(store, "moodle", locks.Limit(1)))
		fences, letGo, ended := holding(t, moodle.Hold, "requests")
		<-fences
		waited := make(chan time.Duration, 1)
		go func() {
			ways.Must(0, moodle.Hold(ctx, "requests", func(_ context.Context, permit *locks.Lease) error {
				waited <- permit.Waited
				return nil
			}))
		}()
		time.Sleep(10 * time.Millisecond)
		inLine := time.Now()
		time.Sleep(50 * time.Millisecond)
		stood := time.Since(inLine)
		close(letGo)
		ways.Must(0, <-ended)
		if got := <-waited; got < stood {
			t.Fatalf("waited %v, though it stood in line %v", got, stood)
		}
	})
}

// contract: LOCK-07
func TestASemaphoreOpenedAgainWithAnotherLimitIsRefusedAtTheOpenAndWithTheSameLimitIsNot(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ways.Must(locks.OpenSemaphore(store, "gpu", locks.Limit(1)))
		ways.Must(locks.OpenSemaphore(store, "gpu", locks.Limit(1)))
		_, err := locks.OpenSemaphore(store, "gpu", locks.Limit(3))
		is(t, err, tinystore.ErrInvalid)
		if want := "semaphore gpu: open in this program with a limit of 1, not 3"; !strings.Contains(err.Error(), want) {
			t.Fatalf("got %q, want it to say %q", err, want)
		}
		ways.Must(locks.OpenSemaphore(store, "tpu", locks.Limit(3)))
	})
}

// served is the order holds of a key were given their places, by name.
type served struct {
	mu    sync.Mutex
	names []string
}

func (s *served) add(name string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.names = append(s.names, name)
}

func (s *served) now() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return slices.Clone(s.names)
}

func (s *served) until(t *testing.T, count int) []string {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for len(s.now()) < count {
		if time.Now().After(deadline) {
			t.Fatalf("served %v, waiting for %d", s.now(), count)
		}
		time.Sleep(5 * time.Millisecond)
	}
	return s.now()
}

// contract: LOCK-09
func TestAnUrgentHoldTakesTheFirstPlaceFreedBeforeTheBackgroundHoldsThatCameFirst(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		cabinet := ways.Must(locks.OpenSemaphore(store, "cabinet", locks.Limit(2)))
		order := &served{}
		letGo := map[string]chan struct{}{}
		ended := make(chan error, 5)
		hold := func(name string, opts ...locks.HoldOption) {
			letGo[name] = make(chan struct{})
			release := letGo[name]
			go func() {
				ended <- cabinet.Hold(ctx, "requests", func(context.Context, *locks.Lease) error {
					order.add(name)
					<-release
					return nil
				}, append(opts, locks.Wait(time.Minute))...)
			}()
		}
		hold("b1", locks.InBackground())
		hold("b2", locks.InBackground())
		order.until(t, 2)
		hold("b3", locks.InBackground())
		hold("b4", locks.InBackground())
		time.Sleep(50 * time.Millisecond)
		hold("urgent")
		time.Sleep(50 * time.Millisecond)
		close(letGo["b1"])
		if got := order.until(t, 3); got[2] != "urgent" {
			t.Fatalf("served %v: the first place freed goes to the urgent hold", got)
		}
		close(letGo["b2"])
		order.until(t, 4)
		close(letGo["urgent"])
		order.until(t, 5)
		close(letGo["b3"])
		close(letGo["b4"])
		for range 5 {
			ways.Must(0, <-ended)
		}
	})
}

// contract: LOCK-10
func TestBackgroundHoldsTakeNoMoreThanTheirPlacesAndAnUrgentHoldFindsTheRestAtOnce(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		lane := ways.Must(locks.OpenSemaphore(store, "lane", locks.Limit(2), locks.Background(1)))
		background := func(ctx context.Context, key any, fn func(context.Context, *locks.Lease) error, opts ...locks.HoldOption) error {
			return lane.Hold(ctx, key, fn, append(opts, locks.InBackground())...)
		}
		fences, letGo, ended := holding(t, background, "requests")
		<-fences
		if ways.Must(lane.TryHold(ctx, "requests", nothing, locks.InBackground())) {
			t.Fatal("a second background hold took a place past the background's one")
		}
		if !ways.Must(lane.TryHold(ctx, "requests", nothing)) {
			t.Fatal("an urgent hold did not find the place left")
		}
		close(letGo)
		ways.Must(0, <-ended)
		_, err := locks.OpenSemaphore(store, "lane", locks.Limit(2))
		is(t, err, tinystore.ErrInvalid)
		if want := "with a background of 1, not none"; !strings.Contains(err.Error(), want) {
			t.Fatalf("got %q, want it to say %q", err, want)
		}
		_, err = locks.OpenSemaphore(store, "wide", locks.Limit(2), locks.Background(3))
		is(t, err, tinystore.ErrInvalid)
	})
}

// contract: LOCK-11
func TestAHoldsTimeoutCountsFromItsGrantAndEndsTheHoldLost(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		lane := ways.Must(locks.OpenSemaphore(store, "timed", locks.Limit(1)))
		fences, letGo, firstEnded := holding(t, lane.Hold, "requests")
		<-fences
		ended := make(chan error, 1)
		ran := make(chan time.Duration, 1)
		go func() {
			ended <- lane.Hold(ctx, "requests", func(ctx context.Context, _ *locks.Lease) error {
				granted := time.Now()
				<-ctx.Done()
				ran <- time.Since(granted)
				return context.Cause(ctx)
			}, locks.Wait(time.Minute), locks.Timeout(300*time.Millisecond))
		}()
		// the place is held twice the timeout: counted from the ask, the hold would end as it is granted
		time.Sleep(600 * time.Millisecond)
		close(letGo)
		ways.Must(0, <-firstEnded)
		ways.Must(0, lane.Hold(ctx, "requests", nothing, locks.Wait(5*time.Second)))
		is(t, <-ended, tinystore.ErrLost)
		if held := <-ran; held < 250*time.Millisecond {
			t.Fatalf("the hold ran %v of its 300ms timeout: counted from its ask, not its grant", held)
		}
		err := lane.Hold(ctx, "requests", nothing, locks.Timeout(time.Microsecond))
		is(t, err, tinystore.ErrInvalid)
		if want := "a timeout of 1µs, not 1 ms or more"; !strings.Contains(err.Error(), want) {
			t.Fatalf("got %q, want it to say %q", err, want)
		}
	})
}

// contract: LOCK-12, LOCK-13, LOCK-14
func TestFailedHoldsInARowPutTheKeyDownAndOneHoldAfterThePausePutsItUp(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		flaky := ways.Must(locks.OpenSemaphore(store, "flaky", locks.Limit(1), locks.Breaker(2, 300*time.Millisecond)))
		badGateway := errors.New("the cabinet answered 502")
		failing := func(context.Context, *locks.Lease) error { return badGateway }
		if state := ways.Must(flaky.State(ctx, "requests")); !state.Up {
			t.Fatalf("down before any hold: %+v", state)
		}
		for range 2 {
			if err := flaky.Hold(ctx, "requests", failing); !errors.Is(err, badGateway) {
				t.Fatalf("got %v, want fn's own error", err)
			}
		}
		err := flaky.Hold(ctx, "requests", nothing)
		is(t, err, tinystore.ErrUnavailable)
		if want := "after 2 failed holds in a row"; !strings.Contains(err.Error(), want) {
			t.Fatalf("got %q, want it to say %q", err, want)
		}
		down := ways.Must(flaky.State(ctx, "requests"))
		if down.Up || down.DownSince.IsZero() || !down.LastFailure.Equal(down.DownSince) {
			t.Fatalf("not down since its last failure: %+v", down)
		}
		if down.DownSince.Location() != time.UTC || down.LastFailure.Location() != time.UTC {
			t.Fatalf("times not in UTC: %+v", down)
		}
		time.Sleep(350 * time.Millisecond)
		ways.Must(0, flaky.Hold(ctx, "requests", nothing))
		if up := ways.Must(flaky.State(ctx, "requests")); !up.Up || !up.DownSince.IsZero() || up.Holding != 0 {
			t.Fatalf("not up after the hold that went through: %+v", up)
		}
		notFound := errors.New("no such student")
		for range 2 {
			err := flaky.Hold(ctx, "requests", func(_ context.Context, lease *locks.Lease) error {
				lease.Succeeded()
				return notFound
			})
			if !errors.Is(err, notFound) {
				t.Fatalf("got %v, want fn's own error", err)
			}
		}
		if state := ways.Must(flaky.State(ctx, "requests")); !state.Up {
			t.Fatalf("down after holds that said they succeeded: %+v", state)
		}
		_, err = locks.OpenSemaphore(store, "flaky", locks.Limit(1))
		if want := "with a breaker of 2 failures and 300ms, not none"; err == nil || !strings.Contains(err.Error(), want) {
			t.Fatalf("got %v, want it to say %q", err, want)
		}
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
