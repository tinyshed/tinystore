package kv_test

import (
	"context"
	"errors"
	"math"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/ways"
	"tinyshed.org/tinystore/kv"
)

type Session struct {
	Device string `json:"device"`
	User   int    `json:"user"`
}

func check(t *testing.T, err error) {
	t.Helper()
	if err != nil {
		t.Fatal(err)
	}
}

func is(t *testing.T, err error, kind error) {
	t.Helper()
	if !errors.Is(err, kind) {
		t.Fatalf("got %v, want %v", err, kind)
	}
}

func TestAValueComesBackAsItWentInUnderItsOwner(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		sessions := ways.Must(kv.OpenBucket[Session](store, "sessions"))
		check(t, sessions.Under(7).Set(ctx, "token", Session{"phone", 1}))
		got, found := ways.Must2(sessions.Under(7).Get(ctx, "token"))
		if !found || got != (Session{"phone", 1}) {
			t.Fatalf("got %v %v", got, found)
		}
		if !ways.Must(sessions.Under("7").Has(ctx, "token")) {
			t.Fatal("an owner 7 and \"7\" are one branch")
		}
		if _, found := ways.Must2(sessions.Get(ctx, "token")); found {
			t.Fatal("the root holds nothing")
		}
		if _, found := ways.Must2(sessions.Under(8).Get(ctx, "token")); found {
			t.Fatal("another owner holds nothing")
		}
	})
}

func TestAStaleVersionIsAConflictNamingItsBucketAndKey(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		sessions := ways.Must(kv.OpenBucket[Session](store, "sessions"))
		check(t, sessions.Set(ctx, "k", Session{"a", 1}))
		first, _ := ways.Must2(sessions.Entry(ctx, "k"))
		check(t, sessions.Set(ctx, "k", Session{"b", 2}, kv.IfVersion(first.Version)))
		err := sessions.Set(ctx, "k", Session{"c", 3}, kv.IfVersion(first.Version))
		is(t, err, tinystore.ErrConflict)
		if !strings.Contains(err.Error(), `kv bucket sessions: key "k"`) {
			t.Fatalf("the conflict names %q", err)
		}
		var e *tinystore.Error
		if !errors.As(err, &e) || e.Kind != tinystore.Conflict {
			t.Fatalf("errors.As gives %v", e)
		}
		got, _ := ways.Must2(sessions.Get(ctx, "k"))
		if got.Device != "b" {
			t.Fatalf("got %v", got)
		}
	})
}

func TestAnExpiryHoldsTheKeyUntilItsTimeAndExpireMovesItEitherWay(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		drafts := ways.Must(kv.OpenBucket[string](store, "drafts"))
		check(t, drafts.Set(ctx, "d", "hello", kv.TTL(time.Hour)))
		entry, _ := ways.Must2(drafts.Entry(ctx, "d"))
		if !entry.ExpiresAt.After(time.Now().Add(59 * time.Minute)) {
			t.Fatalf("expires at %v", entry.ExpiresAt)
		}
		check(t, drafts.Expire(ctx, "d", 2*time.Hour))
		entry, _ = ways.Must2(drafts.Entry(ctx, "d"))
		if !entry.ExpiresAt.After(time.Now().Add(119 * time.Minute)) {
			t.Fatalf("expires at %v", entry.ExpiresAt)
		}
		check(t, drafts.ExpireAt(ctx, "d", time.Now().Add(time.Minute)))
		entry, _ = ways.Must2(drafts.Entry(ctx, "d"))
		if !entry.ExpiresAt.Before(time.Now().Add(61 * time.Second)) {
			t.Fatalf("expires at %v", entry.ExpiresAt)
		}
		check(t, drafts.Set(ctx, "gone", "x", kv.ExpiresAt(time.Now().Add(-time.Second))))
		if _, found := ways.Must2(drafts.Get(ctx, "gone")); found {
			t.Fatal("a key past its expiry holds nothing")
		}
	})
}

func TestCreateAndAddWriteAKeyOnlyWhereNoneIsAndSaySo(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		seen := ways.Must(kv.OpenBucket[struct{}](store, "stripe-events", kv.TTL(7*24*time.Hour)))
		if !ways.Must(seen.Add(ctx, "evt_1")) || ways.Must(seen.Add(ctx, "evt_1")) {
			t.Fatal("add says whether it added")
		}
		if !ways.Must(seen.Has(ctx, "evt_1")) {
			t.Fatal("a set holds its member")
		}
		names := ways.Must(kv.OpenBucket[string](store, "names"))
		if !ways.Must(names.Create(ctx, "ada", "Lovelace")) || ways.Must(names.Create(ctx, "ada", "Byron")) {
			t.Fatal("create says whether it created")
		}
		if got, _ := ways.Must2(names.Get(ctx, "ada")); got != "Lovelace" {
			t.Fatalf("got %q", got)
		}
	})
}

func TestTakeReadsAndBurnsAndDeleteSaysWhetherTheKeyWasThere(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		codes := ways.Must(kv.OpenBucket[int64](store, "login-codes", kv.TTL(15*time.Minute)))
		check(t, codes.Set(ctx, "123456", 42))
		if got, found := ways.Must2(codes.Take(ctx, "123456")); !found || got != 42 {
			t.Fatalf("took %v %v", got, found)
		}
		if _, found := ways.Must2(codes.Take(ctx, "123456")); found {
			t.Fatal("a code is taken once")
		}
		check(t, codes.Set(ctx, "654321", 7))
		if !ways.Must(codes.Delete(ctx, "654321")) || ways.Must(codes.Delete(ctx, "654321")) {
			t.Fatal("delete says whether the key was there")
		}
	})
}

func TestClearEmptiesABranchAndThoseUnderIt(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		carts := ways.Must(kv.OpenBucket[int64](store, "carts"))
		check(t, carts.Under("u1").Set(ctx, "a", 1))
		check(t, carts.Under("u1", "saved").Set(ctx, "b", 2))
		check(t, carts.Under("u2").Set(ctx, "a", 3))
		check(t, carts.Under("u1").Clear(ctx))
		if _, found := ways.Must2(carts.Under("u1").Get(ctx, "a")); found {
			t.Fatal("the branch is empty")
		}
		if _, found := ways.Must2(carts.Under("u1", "saved").Get(ctx, "b")); found {
			t.Fatal("a branch under it is empty")
		}
		if got, _ := ways.Must2(carts.Under("u2").Get(ctx, "a")); got != 3 {
			t.Fatal("another branch keeps its keys")
		}
	})
}

func TestListReadsAPageInTheOrderOfTheKeysAndAllWalksEveryPage(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		pages := ways.Must(kv.OpenBucket[int64](store, "pages")).Under("p")
		for i := range 25 {
			check(t, pages.Set(ctx, keyOf(i), int64(i)))
		}
		first := ways.Must(pages.List(ctx, kv.Limit(10)))
		if len(first.Entries) != 10 || first.Entries[0].Key != "k00" || first.Next != "k09" {
			t.Fatalf("first page %v next %q", first.Entries, first.Next)
		}
		second := ways.Must(pages.List(ctx, kv.Limit(10), kv.After(first.Next)))
		if second.Entries[0].Key != "k10" {
			t.Fatalf("second page starts at %q", second.Entries[0].Key)
		}
		var walked []int64
		for entry, err := range pages.All(ctx, kv.Limit(7)) {
			check(t, err)
			walked = append(walked, entry.Value)
		}
		if len(walked) != 25 || walked[24] != 24 {
			t.Fatalf("walked %v", walked)
		}
	})
}

func keyOf(i int) string {
	return "k" + string(rune('0'+i/10)) + string(rune('0'+i%10))
}

func TestTTLAndIdleAreOneOrTheOtherAndANameIsCheckedBeforeAnythingLeaves(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		_, err := kv.OpenBucket[string](store, "both", kv.TTL(time.Hour), kv.Idle(time.Hour))
		is(t, err, tinystore.ErrInvalid)
		_, err = kv.OpenBucket[string](store, "Bad Name")
		is(t, err, tinystore.ErrInvalid)
		_, err = kv.OpenBucket[string](store, "negative", kv.TTL(-time.Second))
		is(t, err, tinystore.ErrInvalid)
	})
}

func TestEachTypeKeepsItsValuesAsTheCoreKeepsARustType(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		strs := ways.Must(kv.OpenBucket[string](store, "strings"))
		check(t, strs.Set(ctx, "s", "héllo 😀"))
		if got, _ := ways.Must2(strs.Get(ctx, "s")); got != "héllo 😀" {
			t.Fatalf("got %q", got)
		}
		raw := ways.Must(kv.OpenBucket[[]byte](store, "bytes"))
		check(t, raw.Set(ctx, "b", []byte{0, 255, 7}))
		if got, _ := ways.Must2(raw.Get(ctx, "b")); string(got) != string([]byte{0, 255, 7}) {
			t.Fatalf("got %v", got)
		}
		floats := ways.Must(kv.OpenBucket[float64](store, "floats"))
		check(t, floats.Set(ctx, "f", math.Copysign(0, -1)))
		if got, _ := ways.Must2(floats.Get(ctx, "f")); !math.Signbit(got) {
			t.Fatal("-0 keeps its sign")
		}
		flags := ways.Must(kv.OpenBucket[bool](store, "flags"))
		check(t, flags.Set(ctx, "on", true))
		if got, _ := ways.Must2(flags.Get(ctx, "on")); !got {
			t.Fatal("true reads back true")
		}
		panels := ways.Must(kv.OpenBucket[[]Session](store, "panels"))
		check(t, panels.Set(ctx, "p", []Session{{"a", 1}, {"b", 2}}))
		if got, _ := ways.Must2(panels.Get(ctx, "p")); len(got) != 2 || got[1] != (Session{"b", 2}) {
			t.Fatalf("got %v", got)
		}
		check(t, strs.Set(ctx, "mixed", "text"))
		mixed := ways.Must(kv.OpenBucket[int64](store, "strings"))
		_, _, err := mixed.Get(ctx, "mixed")
		is(t, err, tinystore.ErrCorrupt)
		// a uint64 past an int64 is kept, as Rust keeps a u64
		wide := ways.Must(kv.OpenBucket[uint64](store, "wide"))
		check(t, wide.Set(ctx, "j", math.MaxUint64))
		if got, _ := ways.Must2(wide.Get(ctx, "j")); got != math.MaxUint64 {
			t.Fatalf("the largest uint64 reads back %d", got)
		}
	})
}

func TestAddGivesTheNewCountEachBranchApartAndAnAbsentCounterIsZero(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		attempts := ways.Must(kv.OpenCounters(store, "login-attempts", kv.TTL(15*time.Minute)))
		if ways.Must(attempts.Add(ctx, "10.0.0.1", 1)) != 1 || ways.Must(attempts.Add(ctx, "10.0.0.1", 4)) != 5 {
			t.Fatal("add gives the new count")
		}
		if ways.Must(attempts.Under("eu").Add(ctx, "10.0.0.1", 1)) != 1 {
			t.Fatal("a branch counts apart")
		}
		if ways.Must(attempts.Get(ctx, "10.0.0.2")) != 0 {
			t.Fatal("an absent counter is 0")
		}
		if !ways.Must(attempts.Delete(ctx, "10.0.0.1")) || ways.Must(attempts.Get(ctx, "10.0.0.1")) != 0 {
			t.Fatal("delete removes the counter")
		}
		check(t, attempts.Clear(ctx))
		if ways.Must(attempts.Under("eu").Get(ctx, "10.0.0.1")) != 0 {
			t.Fatal("clear removes the branches under it")
		}
		hits := ways.Must(kv.OpenCounters(store, "page-hits", kv.FlushEvery(time.Second)))
		var adding sync.WaitGroup
		for range 20 {
			adding.Add(1)
			go func() {
				defer adding.Done()
				_, err := hits.Add(ctx, "/", 1)
				if err != nil {
					t.Error(err)
				}
			}()
		}
		adding.Wait()
		if ways.Must(hits.Get(ctx, "/")) != 20 {
			t.Fatal("counters kept in memory count all the same")
		}
	})
}

func TestARateLimitLetsItsBurstThroughThenSaysWhenToTryAgain(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		api := ways.Must(kv.OpenRateLimit(store, "api", kv.Rate(2, time.Minute)))
		tenant := api.Under("tenant-7")
		first := ways.Must(tenant.Allow(ctx, "user-1"))
		if !first.OK || first.Left != 1 || !first.RetryAt.IsZero() {
			t.Fatalf("first %+v", first)
		}
		if ways.Must(tenant.Allow(ctx, "user-1")).Left != 0 {
			t.Fatal("the burst is spent")
		}
		third := ways.Must(tenant.Allow(ctx, "user-1"))
		if third.OK || !third.RetryAt.After(time.Now().Add(29*time.Second)) {
			t.Fatalf("third %+v", third)
		}
		if !ways.Must(api.Allow(ctx, "user-1")).OK {
			t.Fatal("another branch counts apart")
		}
		_, err := tenant.Allow(ctx, "user-2", 3)
		is(t, err, tinystore.ErrInvalid)
		check(t, tenant.Reset(ctx, "user-1"))
		if !ways.Must(tenant.Allow(ctx, "user-1")).OK {
			t.Fatal("a reset key starts afresh")
		}
		_, err = kv.OpenRateLimit(store, "bad")
		is(t, err, tinystore.ErrInvalid)
	})
}

func TestAQuotaCountsInEveryWindowOrInNoneAndGivesUsesBack(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		ai := ways.Must(kv.OpenQuota(store, "ai", kv.Window("daily", 2, 24*time.Hour), kv.Window("weekly", 3, 7*24*time.Hour)))
		first := ways.Must(ai.Allow(ctx, "user-1"))
		daily := first.Windows["daily"]
		if !first.OK || daily.Used != 1 || daily.Limit != 2 || daily.Left != 1 || first.Windows["weekly"].Left != 2 {
			t.Fatalf("first %+v", first)
		}
		if !daily.ResetsAt.After(time.Now()) {
			t.Fatal("a window resets later")
		}
		ways.Must(ai.Allow(ctx, "user-1"))
		if ways.Must(ai.Allow(ctx, "user-1")).OK {
			t.Fatal("a full window refuses")
		}
		check(t, ai.Refund(ctx, "user-1"))
		if !ways.Must(ai.Allow(ctx, "user-1")).OK {
			t.Fatal("a refund gives a use back")
		}
		check(t, ai.Reset(ctx, "user-1"))
		if ways.Must(ai.Peek(ctx, "user-1")).Windows["daily"].Used != 0 {
			t.Fatal("a reset forgets the uses")
		}
	})
}

type Receipt struct {
	Receipt string `json:"receipt"`
}

func TestARunKeepsItsAnswerAndAnErrorKeepsNothing(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		charges := ways.Must(kv.OpenOnce[Receipt](store, "charges"))
		var ran atomic.Int32
		charge := func(context.Context) (Receipt, error) {
			n := ran.Add(1)
			return Receipt{"r-" + string(rune('0'+n))}, nil
		}
		if ways.Must(charges.Run(ctx, "order-1", charge)).Receipt != "r-1" || ways.Must(charges.Run(ctx, "order-1", charge)).Receipt != "r-1" {
			t.Fatal("the second run gets the first's answer")
		}
		if ran.Load() != 1 {
			t.Fatal("the function ran once")
		}
		if got, found := ways.Must2(charges.Get(ctx, "order-1")); !found || got.Receipt != "r-1" {
			t.Fatal("the answer is kept")
		}
		if !ways.Must(charges.Delete(ctx, "order-1")) || ways.Must(charges.Run(ctx, "order-1", charge)).Receipt != "r-2" {
			t.Fatal("a deleted answer runs again")
		}
		declines := ways.Must(kv.OpenOnce[string](store, "declines", kv.Keep(time.Hour)))
		timedOut := errors.New("the card network timed out")
		_, err := declines.Run(ctx, "order-2", func(context.Context) (string, error) { return "", timedOut })
		if !errors.Is(err, timedOut) {
			t.Fatalf("got %v", err)
		}
		if ways.Must(declines.Run(ctx, "order-2", func(context.Context) (string, error) { return "charged", nil })) != "charged" {
			t.Fatal("an error keeps nothing")
		}
		atOnce := ways.Must(kv.OpenOnce[int](store, "at-once")).Under("shop-1")
		var slow atomic.Int32
		var running sync.WaitGroup
		answers := make([]int, 2)
		for i := range 2 {
			running.Add(1)
			go func() {
				defer running.Done()
				answers[i], err = atOnce.Run(ctx, "order-3", func(context.Context) (int, error) {
					slow.Add(1)
					time.Sleep(50 * time.Millisecond)
					return 99, nil
				})
			}()
		}
		running.Wait()
		if answers[0] != 99 || answers[1] != 99 || slow.Load() != 1 {
			t.Fatalf("answers %v, ran %d", answers, slow.Load())
		}
	})
}

func TestReadsDecideTheWritesWhichCommitTogether(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		stock := ways.Must(kv.OpenBucket[int64](store, "stock"))
		orders := ways.Must(kv.OpenBucket[string](store, "orders"))
		check(t, stock.Set(ctx, "sku-1", 1))
		check(t, store.Tx(ctx, func(tx *tinystore.Tx) error {
			left, _, err := stock.In(tx).Get(ctx, "sku-1")
			if err != nil || left < 1 {
				return err
			}
			if err := stock.In(tx).Set(ctx, "sku-1", left-1); err != nil {
				return err
			}
			if after, _, _ := stock.In(tx).Get(ctx, "sku-1"); after != 0 {
				t.Errorf("a read sees the transaction's own write, %d", after)
			}
			return orders.In(tx).Set(ctx, "o-1", "sku-1")
		}))
		if got, _ := ways.Must2(stock.Get(ctx, "sku-1")); got != 0 {
			t.Fatalf("stock %d", got)
		}
		if got, _ := ways.Must2(orders.Get(ctx, "o-1")); got != "sku-1" {
			t.Fatalf("order %q", got)
		}
		contended := ways.Must(kv.OpenBucket[int64](store, "contended"))
		check(t, contended.Set(ctx, "sku-2", 10))
		runs := 0
		check(t, store.Tx(ctx, func(tx *tinystore.Tx) error {
			runs++
			left, _, err := contended.In(tx).Get(ctx, "sku-2")
			if err != nil {
				return err
			}
			if runs == 1 {
				if err := contended.Set(ctx, "sku-2", 5); err != nil {
					return err
				}
			}
			return contended.In(tx).Set(ctx, "sku-2", left-1)
		}))
		if got, _ := ways.Must2(contended.Get(ctx, "sku-2")); runs != 2 || got != 4 {
			t.Fatalf("runs %d, left %d", runs, got)
		}
		notes := ways.Must(kv.OpenBucket[string](store, "notes"))
		changed := errors.New("changed my mind")
		err := store.Tx(ctx, func(tx *tinystore.Tx) error {
			if err := notes.In(tx).Set(ctx, "n", "draft"); err != nil {
				return err
			}
			return changed
		})
		if !errors.Is(err, changed) {
			t.Fatalf("got %v", err)
		}
		if _, found := ways.Must2(notes.Get(ctx, "n")); found {
			t.Fatal("an error writes nothing")
		}
	})
}

func TestTakeCreateDeleteAndClearAnswerAsTheCommitWillLeaveTheKeys(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		codes := ways.Must(kv.OpenBucket[int64](store, "tx-codes"))
		check(t, codes.Set(ctx, "c1", 1))
		check(t, codes.Under("u1").Set(ctx, "c2", 2))
		attempts := ways.Must(kv.OpenCounters(store, "tx-attempts"))
		check(t, store.Tx(ctx, func(tx *tinystore.Tx) error {
			in := codes.In(tx)
			if got, found, _ := in.Take(ctx, "c1"); !found || got != 1 {
				t.Errorf("took %d %v", got, found)
			}
			if _, found, _ := in.Get(ctx, "c1"); found {
				t.Error("a taken key holds nothing")
			}
			if created, _ := in.Create(ctx, "c1", 3); !created {
				t.Error("create writes where none is")
			}
			if created, _ := in.Create(ctx, "c1", 4); created {
				t.Error("create writes nowhere a key is")
			}
			if deleted, _ := in.Delete(ctx, "missing"); deleted {
				t.Error("delete says the key was not there")
			}
			if err := in.Under("u1").Clear(ctx); err != nil {
				return err
			}
			if _, found, _ := in.Under("u1").Get(ctx, "c2"); found {
				t.Error("a cleared branch holds nothing")
			}
			_, err := attempts.In(tx).Add(ctx, "ip", 2)
			return err
		}))
		if got, _ := ways.Must2(codes.Get(ctx, "c1")); got != 3 {
			t.Fatalf("c1 %d", got)
		}
		if ways.Must(attempts.Get(ctx, "ip")) != 2 {
			t.Fatal("the counter added with the commit")
		}
	})
}

func TestAHandleOfAnotherStoreIsRefused(t *testing.T) {
	ways.EachWay(t, func(t *testing.T, way ways.Way) {
		ctx := context.Background()
		store := ways.Open(t, way)
		other := ways.Open(t, way)
		theirs := ways.Must(kv.OpenBucket[int64](other, "stock"))
		err := store.Tx(ctx, func(tx *tinystore.Tx) error {
			_, _, err := theirs.In(tx).Get(ctx, "sku-1")
			return err
		})
		is(t, err, tinystore.ErrInvalid)
	})
}
