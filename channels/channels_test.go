package channels_test

import (
	"context"
	"errors"
	"reflect"
	"sync"
	"testing"
	"testing/fstest"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/channels"
	"github.com/tinyshed/tinystore/internal/ways"
	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/sqldb"
)

type chat struct {
	Sender string `json:"sender"`
	Text   string `json:"text"`
}

// got is what handlers received, read while they write it.
type got[T any] struct {
	mu   sync.Mutex
	seen []T
}

func (g *got[T]) add(value T) {
	g.mu.Lock()
	defer g.mu.Unlock()
	g.seen = append(g.seen, value)
}

func (g *got[T]) all() []T {
	g.mu.Lock()
	defer g.mu.Unlock()
	return append([]T(nil), g.seen...)
}

func TestASubscriberReceivesWhatIsPublishedFromNowOnInOrderUntilItStops(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		chats := ways.Must(channels.OpenChannel[chat](store, "chat"))
		ways.Must(0, chats.Publish(ctx, chat{"ann", "before anyone listened"}))
		var received got[chat]
		sub := ways.Must(chats.Subscribe(ctx, func(_ context.Context, m channels.Message[chat]) error {
			received.add(m.Value)
			return nil
		}))
		for _, text := range []string{"a", "b", "c"} {
			ways.Must(0, chats.Publish(ctx, chat{"ann", text}))
		}
		ways.Until(t, func() bool { return len(received.all()) == 3 })
		if want := []chat{{"ann", "a"}, {"ann", "b"}, {"ann", "c"}}; !reflect.DeepEqual(received.all(), want) {
			t.Fatalf("received %v", received.all())
		}
		ways.Must(0, sub.Stop(ctx))
		ways.Must(0, chats.Publish(ctx, chat{"ann", "after the stop"}))
		time.Sleep(50 * time.Millisecond)
		if len(received.all()) != 3 {
			t.Fatalf("a stopped subscription received %v", received.all())
		}
	})
}

func TestASubscriptionTakesItsExactBranchAndRecursiveTheBranchesBelowToo(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		orders := ways.Must(channels.OpenChannel[int](store, "orders"))
		var exact got[int]
		var below got[channels.Message[int]]
		first := ways.Must(orders.Under("eu").Subscribe(ctx, func(_ context.Context, m channels.Message[int]) error {
			exact.add(m.Value)
			return nil
		}))
		second := ways.Must(orders.Under("eu").Subscribe(ctx, func(_ context.Context, m channels.Message[int]) error {
			below.add(m)
			return nil
		}, channels.Recursive()))
		ways.Must(0, orders.Publish(ctx, 1))
		ways.Must(0, orders.Under("eu").Publish(ctx, 2))
		ways.Must(0, orders.Under("eu", "shop-7").Publish(ctx, 3))
		ways.Until(t, func() bool { return len(below.all()) == 2 })
		time.Sleep(50 * time.Millisecond)
		if !reflect.DeepEqual(exact.all(), []int{2}) {
			t.Fatalf("exact %v", exact.all())
		}
		if all := below.all(); all[0].Value != 2 || all[1].Value != 3 || !reflect.DeepEqual(all[1].Branch, []string{"eu", "shop-7"}) {
			t.Fatalf("below %+v", all)
		}
		ways.Must(0, first.Stop(ctx))
		ways.Must(0, second.Stop(ctx))
	})
}

func TestALoopReceivesTheMessagesPublishedFromItsFirstTurnAndLeavingItEndsIt(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		ticks := ways.Must(channels.OpenChannel[int](store, "ticks"))
		go func() {
			for n := 1; ctx.Err() == nil; n++ {
				_ = ticks.Publish(ctx, n)
				time.Sleep(20 * time.Millisecond)
			}
		}()
		var seen []int
		for m, err := range ticks.Each(ctx) {
			if err != nil {
				t.Fatal(err)
			}
			seen = append(seen, m.Value)
			if len(seen) == 2 {
				break
			}
		}
		if len(seen) != 2 || seen[1] != seen[0]+1 {
			t.Fatalf("seen %v", seen)
		}
	})
}

func TestATransactionsMessagesAreHandedOverOnceItCommitsAndNeverWhenItRollsBack(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		events := ways.Must(channels.OpenChannel[string](store, "events"))
		var received got[string]
		sub := ways.Must(events.Subscribe(ctx, func(_ context.Context, m channels.Message[string]) error {
			received.add(m.Value)
			return nil
		}, channels.Recursive()))
		orders := ways.Must(kv.OpenBucket[string](store, "orders"))
		refused := errors.New("the order is refused")
		err := store.Tx(ctx, func(tx *tinystore.Tx) error {
			if err := orders.In(tx).Set(ctx, "1", "order"); err != nil {
				return err
			}
			if err := events.In(tx).Under("1").Publish(ctx, "created 1"); err != nil {
				return err
			}
			return refused
		})
		if !errors.Is(err, refused) {
			t.Fatalf("tx %v", err)
		}
		ways.Must(0, store.Tx(ctx, func(tx *tinystore.Tx) error {
			if err := orders.In(tx).Set(ctx, "2", "order"); err != nil {
				return err
			}
			return events.In(tx).Under("2").Publish(ctx, "created 2")
		}))
		app := ways.Must(sqldb.Open(ctx, store, "app", sqldb.Migrations(fstest.MapFS{
			"0001_notes.sql": {Data: []byte("create table notes (id integer primary key)")},
		})))
		err = app.Tx(ctx, func(tx *sqldb.Tx) error {
			if _, err := tx.Exec(ctx, "insert into notes (id) values (1)"); err != nil {
				return err
			}
			if err := events.In(tx).Publish(ctx, "rolled back"); err != nil {
				return err
			}
			return refused
		})
		if !errors.Is(err, refused) {
			t.Fatalf("db tx %v", err)
		}
		ways.Must(0, app.Tx(ctx, func(tx *sqldb.Tx) error {
			if _, err := tx.Exec(ctx, "insert into notes (id) values (2)"); err != nil {
				return err
			}
			return events.In(tx).Under("note").Publish(ctx, "noted")
		}))
		ways.Until(t, func() bool { return len(received.all()) == 2 })
		time.Sleep(50 * time.Millisecond)
		if !reflect.DeepEqual(received.all(), []string{"created 2", "noted"}) {
			t.Fatalf("received %v", received.all())
		}
		ways.Must(0, sub.Stop(ctx))
	})
}

func TestAHandlerThatFailsIsToldTheNextMessageFollowsAndStopInsideItAnswersAtOnce(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		numbers := ways.Must(channels.OpenChannel[int](store, "numbers"))
		var received got[int]
		var failures got[string]
		var sub *channels.Subscription
		ready := make(chan struct{})
		sub = ways.Must(numbers.Subscribe(ctx, func(ctx context.Context, m channels.Message[int]) error {
			<-ready
			if m.Value == 2 {
				return errors.New("two is refused")
			}
			received.add(m.Value)
			if m.Value == 3 {
				return sub.Stop(ctx)
			}
			return nil
		}, channels.OnError(func(err error, _ channels.Message[int]) { failures.add(err.Error()) })))
		close(ready)
		for _, value := range []int{1, 2, 3, 4} {
			ways.Must(0, numbers.Publish(ctx, value))
		}
		ways.Until(t, func() bool { return len(received.all()) == 2 })
		time.Sleep(50 * time.Millisecond)
		if !reflect.DeepEqual(received.all(), []int{1, 3}) || !reflect.DeepEqual(failures.all(), []string{"two is refused"}) {
			t.Fatalf("received %v, failures %v", received.all(), failures.all())
		}
	})
}

func TestASlowHandlerFallsBehindAndIsToldHowManyItMissed(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		photos := ways.Must(channels.OpenChannel[string](store, "photos"))
		photo := string(make([]byte, 4096))
		released := make(chan struct{})
		var mu sync.Mutex
		received, missed := 0, 0
		sub := ways.Must(photos.Subscribe(ctx, func(context.Context, channels.Message[string]) error {
			mu.Lock()
			received++
			mu.Unlock()
			<-released
			return nil
		}, channels.OnMissed(func(told channels.Missed) {
			mu.Lock()
			missed += told.Count
			mu.Unlock()
		})))
		const published = 3000
		for range published {
			ways.Must(0, photos.Publish(ctx, photo))
		}
		close(released)
		total := func() (int, int) {
			mu.Lock()
			defer mu.Unlock()
			return received, missed
		}
		deadline := time.Now().Add(30 * time.Second)
		for r, m := total(); r+m != published && time.Now().Before(deadline); r, m = total() {
			time.Sleep(20 * time.Millisecond)
		}
		if r, m := total(); m == 0 || r+m != published {
			t.Fatalf("received %d and missed %d of %d", r, m, published)
		}
		ways.Must(0, sub.Stop(ctx))
	})
}

func TestNamesAreCheckedBeforeAnythingIsSent(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		_, err := channels.OpenChannel[chat](store, "Chat")
		if !errors.Is(err, tinystore.ErrInvalid) {
			t.Fatalf("got %v", err)
		}
	})
}
