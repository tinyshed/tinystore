package blobs_test

import (
	"bytes"
	"context"
	"testing"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/blobs"
	"tinyshed.org/tinystore/internal/ways"
)

// A set of files as a cache, as docs/guides/blobs/caching.md has it, through
// each way a store is reached.

func listed(t *testing.T, files *blobs.Files) []string {
	t.Helper()
	page := ways.Must(files.List(context.Background()))
	paths := make([]string, len(page.Files))
	for i, file := range page.Files {
		paths[i] = file.Path
	}
	return paths
}

// contract: BLOB-18, BLOB-20
func TestAWritePastMaxSizeEvictsAndTheCeilingIsTheSets(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		pages := ways.Must(blobs.OpenFiles(store, "pages", blobs.MaxSize(300)))
		for _, path := range []string{"a", "b", "c", "d"} {
			ways.Must(pages.Put(ctx, path, bytes.NewReader(make([]byte, 100))))
		}
		if usage := ways.Must(pages.Usage(ctx)); usage != (blobs.Usage{Count: 3, Size: 300, MaxSize: 300, Evictions: 1}) {
			t.Fatalf("usage %+v", usage)
		}
		if paths := listed(t, pages); len(paths) != 3 || paths[0] != "b" {
			t.Fatalf("listed %v", paths)
		}
		other := ways.Must(blobs.OpenFiles(store, "pages", blobs.MaxSize(1<<10)))
		_, _, err := other.Head(ctx, "b")
		is(t, err, tinystore.ErrConflict)
		_, err = pages.Put(ctx, "big", bytes.NewReader(make([]byte, 301)))
		is(t, err, tinystore.ErrLimit)
	})
}

// contract: BLOB-20, BLOB-24
func TestSetMaxSizeChangesTheCeilingWhenIfCurrentHoldsAndTrimFreesRoom(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		thumbs := ways.Must(blobs.OpenFiles(store, "thumbs", blobs.MaxSize(1<<10)))
		for _, path := range []string{"1", "2", "3"} {
			ways.Must(thumbs.Put(ctx, path, bytes.NewReader(make([]byte, 100))))
		}
		is(t, thumbs.SetMaxSize(ctx, 200, blobs.IfCurrent(2<<10)), tinystore.ErrConflict)
		if err := thumbs.SetMaxSize(ctx, 200, blobs.IfCurrent(1<<10)); err != nil {
			t.Fatal(err)
		}
		if usage := ways.Must(thumbs.Usage(ctx)); usage != (blobs.Usage{Count: 2, Size: 200, MaxSize: 200, Evictions: 1}) {
			t.Fatalf("usage %+v", usage)
		}
		if trimmed := ways.Must(thumbs.Trim(ctx, 100)); trimmed != (blobs.Trim{Evictions: 1, Freed: 100}) {
			t.Fatalf("trimmed %+v", trimmed)
		}
		if err := thumbs.SetMaxSize(ctx, blobs.NoMaxSize); err != nil {
			t.Fatal(err)
		}
		if usage := ways.Must(thumbs.Usage(ctx)); usage.MaxSize != 0 {
			t.Fatalf("usage %+v", usage)
		}
	})
}

// contract: BLOB-23
func TestWhenFullRefuseFailsAWritePastTheCeilingAndRemovesNothing(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		uploads := ways.Must(blobs.OpenFiles(store, "uploads", blobs.MaxSize(150), blobs.WhenFull(blobs.Refuse)))
		ways.Must(uploads.Put(ctx, "a", bytes.NewReader(make([]byte, 100))))
		_, err := uploads.Put(ctx, "b", bytes.NewReader(make([]byte, 100)))
		is(t, err, tinystore.ErrLimit)
		if usage := ways.Must(uploads.Usage(ctx)); usage.Count != 1 {
			t.Fatalf("usage %+v", usage)
		}
		_, err = uploads.Trim(ctx, 0)
		is(t, err, tinystore.ErrInvalid)
	})
}

// contract: BLOB-17
func TestIdleGivesAFileItsTermFromItsLastUseNeverBesideTTL(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		sessions := ways.Must(blobs.OpenFiles(store, "sessions", blobs.Idle(30*24*time.Hour)))
		put := ways.Must(sessions.Put(ctx, "a.json", bytes.NewReader([]byte("{}"))))
		if gap := time.Until(put.Expires) - 30*24*time.Hour; gap > time.Minute || gap < -time.Minute {
			t.Fatalf("expires %v", put.Expires)
		}
		_, err := blobs.OpenFiles(store, "x", blobs.TTL(time.Hour), blobs.Idle(time.Hour))
		is(t, err, tinystore.ErrInvalid)
	})
}
