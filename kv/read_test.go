package kv

import (
	"bytes"
	"errors"
	"fmt"
	"slices"
	"testing"

	"github.com/tinyshed/tinystore"
)

// a Scan reads a branch's own keys, in the byte order of their text, a page
// at a time; the keys of the branches under it are theirs
func TestScanReadsABranchsOwnKeysInPages(t *testing.T) {
	state := openTestState(t, t.TempDir())
	drafts := openTestBucket[string](t, state, "drafts")
	for _, key := range []any{9, 10, 1, "b", "a"} {
		if err := drafts.Of(7).Set(t.Context(), key, "draft"); err != nil {
			t.Fatal(err)
		}
	}
	for _, other := range []*Bucket[string]{drafts, drafts.Of(8), drafts.Of(7, "folder")} {
		if err := other.Set(t.Context(), "elsewhere", "draft"); err != nil {
			t.Fatal(err)
		}
	}

	var keys []string
	query := Query{Limit: 2}
	for pages := 0; ; pages++ {
		page, err := drafts.Of(7).Scan(t.Context(), query)
		if err != nil || pages > 3 {
			t.Fatalf("page %d: %v", pages, err)
		}
		for _, entry := range page.Entries {
			keys = append(keys, entry.Key)
		}
		if !page.More {
			break
		}
		query = page.Next
	}
	if want := []string{"1", "10", "9", "a", "b"}; !slices.Equal(keys, want) {
		t.Fatalf("scanned %v, want %v", keys, want)
	}
	if _, err := drafts.Scan(t.Context(), Query{Limit: maxScanLimit + 1}); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a page over the limit: %v", err)
	}
}

// a page ends before the value that would take it past its bytes, and the
// next begins with that value, so that no key is lost or read twice; a page
// that its values fill exactly keeps them all
func TestAPageEndsBeforeTheValueThatPassesItsBytes(t *testing.T) {
	state := openTestState(t, t.TempDir())
	values := openTestBucket[[]byte](t, state, "values")
	sizes := []int{maxValue, maxValue, maxValue, maxValue, 10, 900_000, 900_000, 900_000, 900_000, 900_000, 10}
	err := state.Tx(t.Context(), func(tx *Tx) error {
		for i, size := range sizes {
			if err := values.WithTx(tx).Set(t.Context(), fmt.Sprintf("%02d", i), bytes.Repeat([]byte{byte(i)}, size)); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}

	for limit, want := range map[int][][]int{
		0: {sizes[:4], sizes[4:9], sizes[9:]},
		2: {sizes[:2], sizes[2:4], sizes[4:6], sizes[6:8], sizes[8:10], sizes[10:]},
	} {
		var pages [][]int
		query := Query{Limit: limit}
		for {
			page, err := values.Scan(t.Context(), query)
			if err != nil {
				t.Fatal(err)
			}
			pages = append(pages, pageSizes(t, page, len(pages)))
			if !page.More {
				break
			}
			query = page.Next
		}
		if !slices.EqualFunc(pages, want, slices.Equal) {
			t.Fatalf("pages of %d keys held %v, want %v", limit, pages, want)
		}
	}
}

// pageSizes is the size of each value on a page, which the page's keys say
// in order: key 03 holds bytes of 3
func pageSizes(t *testing.T, page Page[[]byte], first int) []int {
	t.Helper()
	sizes, held := []int{}, 0
	for _, entry := range page.Entries {
		if len(entry.Value) > 0 && fmt.Sprintf("%02d", entry.Value[0]) != entry.Key {
			t.Fatalf("page %d: key %s holds the value of %d", first, entry.Key, entry.Value[0])
		}
		sizes = append(sizes, len(entry.Value))
		held += len(entry.Value)
	}
	if held > scanBytes {
		t.Fatalf("page %d holds %d bytes of values, over %d", first, held, scanBytes)
	}
	return sizes
}

// All walks every key of a branch across pages, holds no snapshot between
// them, so a key written after the first page is met, and stops when its loop
// does
func TestAllWalksEveryKeyAPageAtATime(t *testing.T) {
	state := openTestState(t, t.TempDir())
	drafts := openTestBucket[int](t, state, "drafts")
	const keys = maxScanLimit + 500
	err := state.Tx(t.Context(), func(tx *Tx) error {
		for n := range keys {
			if setErr := drafts.WithTx(tx).Of(7).Set(t.Context(), fmt.Sprintf("k%05d", n), n); setErr != nil {
				return setErr
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}

	walked := 0
	for entry, walkErr := range drafts.Of(7).All(t.Context()) {
		if walkErr != nil || entry.Value != walked && entry.Key != "z" {
			t.Fatalf("key %d of the walk: %+v, %v", walked, entry, walkErr)
		}
		if walked == 0 {
			if err = drafts.Of(7).Set(t.Context(), "z", -1); err != nil {
				t.Fatal(err)
			}
		}
		walked++
	}
	if walked != keys+1 {
		t.Fatalf("All met %d keys; want %d and the one written during the walk", walked, keys+1)
	}
	for range drafts.Of(7).All(t.Context()) {
		break
	}
}

// an error about one key names its bucket and the path of owners and key
func TestAnErrorNamesItsBucketAndPath(t *testing.T) {
	state := openTestState(t, t.TempDir())
	drafts := openTestBucket[string](t, state, "drafts")
	entry, err := drafts.Of(7).SetEntry(t.Context(), 1, "draft")
	if err != nil {
		t.Fatal(err)
	}
	if err = drafts.Of(7).Delete(t.Context(), 1); err != nil {
		t.Fatal(err)
	}
	err = drafts.Of(7).Set(t.Context(), 1, "draft", IfVersion(entry.Version))
	var named *KeyError
	if !errors.As(err, &named) || named.Bucket != "drafts" || named.Path != "7/1" ||
		!errors.Is(err, tinystore.ErrConflict) {
		t.Fatalf("a conflict names %+v: %v", named, err)
	}
}
