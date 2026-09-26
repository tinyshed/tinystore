package kv

import (
	"errors"
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
