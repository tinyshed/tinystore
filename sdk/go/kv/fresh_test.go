package kv_test

import (
	"context"
	"errors"
	"sync/atomic"
	"testing"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/ways"
	"tinyshed.org/tinystore/kv"
)

type GradeBook struct {
	User  int   `json:"user"`
	Built int32 `json:"built"`
}

// clocked runs test through each way a store runs on a test's clock: the core
// in this process and a private child.
func clocked(t *testing.T, test func(t *testing.T, store *tinystore.Store)) {
	start := time.Date(2026, 11, 2, 9, 0, 0, 0, time.UTC)
	opens := []struct {
		name  string
		ready bool
		opts  []tinystore.Option
	}{
		{"the core in this process", ways.Library() != "", []tinystore.Option{tinystore.Library(ways.Library())}},
		{"a private child", ways.Binary() != "", []tinystore.Option{tinystore.Private(), tinystore.Binary(ways.Binary())}},
	}
	for _, way := range opens {
		t.Run(way.name, func(t *testing.T) {
			if !way.ready {
				t.Skip("not built here")
			}
			store, err := tinystore.Open(context.Background(), t.TempDir(), append(way.opts, tinystore.Clock(start))...)
			check(t, err)
			defer store.Close()
			test(t, store)
		})
	}
}

// contract: KV-16, KV-17, KV-18
func TestAStaleAnswerIsAnsweredAtOnceWhileItBuildsAndTheLastGoodStaysWhileTheSourceIsDown(t *testing.T) {
	clocked(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		books := ways.Must(kv.OpenOnce[GradeBook](store, "cabinet-books", kv.Fresh(10*time.Minute), kv.Keep(12*time.Hour)))
		var built atomic.Int32
		var down atomic.Bool
		downErr := errors.New("the cabinet is down")
		cabinet := func(context.Context) (GradeBook, error) {
			if down.Load() {
				return GradeBook{}, downErr
			}
			return GradeBook{7, built.Add(1)}, nil
		}
		advance := func(d time.Duration) { check(t, store.Clock().Advance(ctx, d)) }

		first := ways.Must(books.Answer(ctx, "7", cabinet))
		if first != (kv.Answer[GradeBook]{Value: GradeBook{7, 1}}) {
			t.Fatalf("first: %+v", first)
		}
		advance(5 * time.Minute)
		if within := ways.Must(books.Answer(ctx, "7", cabinet)); within != (kv.Answer[GradeBook]{GradeBook{7, 1}, 5 * time.Minute, false}) || built.Load() != 1 {
			t.Fatalf("within fresh: %+v", within)
		}
		advance(6 * time.Minute)
		if stale := ways.Must(books.Answer(ctx, "7", cabinet)); stale != (kv.Answer[GradeBook]{GradeBook{7, 1}, 11 * time.Minute, true}) {
			t.Fatalf("stale: %+v", stale)
		}
		ways.Until(t, func() bool { book, _ := ways.Must2(books.Get(ctx, "7")); return book.Built == 2 })
		if ways.Must(books.Run(ctx, "7", cabinet)).Built != 2 {
			t.Fatal("the new answer, fresh again")
		}

		down.Store(true)
		advance(11 * time.Minute)
		if ways.Must(books.Answer(ctx, "7", cabinet)).Value.Built != 2 {
			t.Fatal("the last good answer")
		}
		advance(time.Minute)
		if !ways.Must(books.Answer(ctx, "7", cabinet)).Stale {
			t.Fatal("still stale while the source is down")
		}
		if book, _ := ways.Must2(books.Get(ctx, "7")); book.Built != 2 {
			t.Fatal("a failed build keeps the kept answer")
		}
		advance(12 * time.Hour)
		if _, err := books.Run(ctx, "7", cabinet); !errors.Is(err, downErr) {
			t.Fatalf("past keep the build's error is the caller's: %v", err)
		}
	})
}

// contract: KV-19
func TestAClearForgetsABranchAndTheBranchesUnderIt(t *testing.T) {
	clocked(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		books := ways.Must(kv.OpenOnce[int](store, "books", kv.Fresh(time.Minute), kv.Keep(time.Hour)))
		answer := func(n int) func(context.Context) (int, error) {
			return func(context.Context) (int, error) { return n, nil }
		}
		ways.Must(books.Under("course-1").Run(ctx, "7", answer(1)))
		ways.Must(books.Under("course-1", "group-2").Run(ctx, "8", answer(2)))
		ways.Must(books.Under("course-2").Run(ctx, "7", answer(3)))
		check(t, books.Under("course-1").Clear(ctx))
		if _, found := ways.Must2(books.Under("course-1", "group-2").Get(ctx, "8")); found {
			t.Fatal("the branch under it is cleared too")
		}
		if kept, _ := ways.Must2(books.Under("course-2").Get(ctx, "7")); kept != 3 {
			t.Fatal("a branch beside stays")
		}
		refused := ways.Must(kv.OpenOnce[int](store, "refused", kv.Fresh(time.Hour), kv.Keep(time.Hour)))
		is(t, func() error { _, err := refused.Run(ctx, "7", answer(1)); return err }(), tinystore.ErrInvalid)
	})
}

// contract: KV-16
func TestAStaleAnswerThatDoesNotDecodeGivesItsBuildBackAtOnce(t *testing.T) {
	clocked(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		books := ways.Must(kv.OpenOnce[GradeBook](store, "cabinet-books", kv.Fresh(10*time.Minute), kv.Keep(12*time.Hour)))
		ways.Must(books.Run(ctx, "7", func(context.Context) (GradeBook, error) { return GradeBook{7, 1}, nil }))
		check(t, store.Clock().Advance(ctx, 11*time.Minute))
		misread := ways.Must(kv.OpenOnce[int](store, "cabinet-books", kv.Fresh(10*time.Minute), kv.Keep(12*time.Hour)))
		if _, err := misread.Fetch(ctx, "7", func(context.Context) (int, error) { return 2, nil }); err == nil {
			t.Fatal("a grade book read as a number")
		}

		// a run once the answer is gone waits for no build the failed fetch was handed
		ways.Must(books.Delete(ctx, "7"))
		waited, cancel := context.WithTimeout(ctx, 5*time.Second)
		defer cancel()
		book, err := books.Run(waited, "7", func(context.Context) (GradeBook, error) { return GradeBook{7, 3}, nil })
		check(t, err)
		if book.Built != 3 {
			t.Fatalf("built: %+v", book)
		}
	})
}

// pastFresh is longer than the fresh of 200 ms the real clock's test opens
// with, so that what was kept is stale after it.
const pastFresh = 300 * time.Millisecond

// contract: KV-16, KV-17
func TestOnTheRealClockAStaleAnswerIsAnsweredAtOnceTheRebuiltOneAfterAndTheLastGoodWhileDown(t *testing.T) {
	ways.EachWay(t, func(t *testing.T, way ways.Way) {
		if way.Name != "a sidecar" && !strings.HasPrefix(way.Name, "a remote server") {
			t.Skip("on a test's clock above")
		}
		store := ways.Open(t, way)
		ctx := context.Background()
		books := ways.Must(kv.OpenOnce[GradeBook](store, "cabinet-books", kv.Fresh(200*time.Millisecond), kv.Keep(time.Hour)))
		var built, asked atomic.Int32
		var down atomic.Bool
		cabinet := func(context.Context) (GradeBook, error) {
			asked.Add(1)
			if down.Load() {
				return GradeBook{}, errors.New("the cabinet is down")
			}
			return GradeBook{7, built.Add(1)}, nil
		}
		kept := func() int32 { book, _ := ways.Must2(books.Get(ctx, "7")); return book.Built }

		if first := ways.Must(books.Fetch(ctx, "7", cabinet)); first.Value != (GradeBook{7, 1}) || first.Stale {
			t.Fatalf("first: %+v", first)
		}
		time.Sleep(pastFresh)
		if stale := ways.Must(books.Fetch(ctx, "7", cabinet)); stale.Value != (GradeBook{7, 1}) || !stale.Stale {
			t.Fatalf("stale: %+v", stale)
		}
		ways.Until(t, func() bool { return kept() == 2 })

		down.Store(true)
		time.Sleep(pastFresh)
		if last := ways.Must(books.Fetch(ctx, "7", cabinet)); last.Value != (GradeBook{7, 2}) || !last.Stale {
			t.Fatalf("the last good answer, stale: %+v", last)
		}
		ways.Until(t, func() bool { return asked.Load() >= 3 })
		if kept() != 2 || ways.Must(books.Fetch(ctx, "7", cabinet)).Value != (GradeBook{7, 2}) || built.Load() != 2 {
			t.Fatalf("a failed build keeps the kept answer: %d, built %d", kept(), built.Load())
		}
	})
}
