package blobs

import (
	"fmt"
	"slices"
	"testing"
	"time"
)

// Scan walks every key under a folder, its sub-folders' included, in the byte
// order of the paths, a page at a time. A folder never meets its neighbours
// whose names it begins.
func TestScanWalksEveryKeyUnderItsFolder(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	for _, key := range []string{
		"users/4/a", "users/42/a", "users/42/photos/1.jpg", "users/42/photos/10.jpg", "users/42/photos/9.jpg",
		"users/42/b", "users/43/a", "users/42-archive/a",
	} {
		mustPut(t, media, key, []byte(key))
	}
	var keys []string
	query := Query{Limit: 2}
	for {
		page, err := media.Of("users", 42).Scan(t.Context(), query)
		if err != nil {
			t.Fatal(err)
		}
		for _, object := range page.Objects {
			keys = append(keys, object.Key)
		}
		if !page.More {
			break
		}
		query = page.Next
	}
	want := []string{"a", "b", "photos/1.jpg", "photos/10.jpg", "photos/9.jpg"}
	if !slices.Equal(keys, want) {
		t.Fatalf("users/42/ holds %v, want %v", keys, want)
	}
	page, err := media.Of("users", 42).Scan(t.Context(), Query{Prefix: "photos/1"})
	if err != nil || len(page.Objects) != 2 || page.More {
		t.Fatalf("the prefix photos/1: %+v, %v", page, err)
	}
	var all []string
	for object, err := range media.All(t.Context(), Query{Prefix: "users/4"}) {
		if err != nil {
			t.Fatal(err)
		}
		all = append(all, object.Key)
	}
	if len(all) != 8 || all[0] != "users/4/a" || all[1] != "users/42-archive/a" || all[7] != "users/43/a" {
		t.Fatalf("the keys starting users/4: %v", all)
	}
}

// Usage counts the objects under a folder and their bytes from their rows,
// after every write, copy, move, delete, expiry and Clear
func TestUsageCountsWhatIsThere(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	files := openTestBucket(t, s, "chat-files")
	mine, theirs := files.Of("users", 1), files.Of("users", 10)
	usageIs := func(bucket *Bucket, objects, bytes int64) {
		t.Helper()
		usage, err := bucket.Usage(t.Context())
		if err != nil || usage != (Usage{Objects: objects, Bytes: bytes}) {
			t.Fatalf("usage %+v, %v; want %d objects, %d bytes", usage, err, objects, bytes)
		}
	}
	mustPut(t, mine, "pending/a", randomBytes(1, 1000), TTL(time.Hour))
	mustPut(t, mine, "pending/b", randomBytes(2, 1<<20))
	mustPut(t, theirs, "x", randomBytes(3, 7))
	usageIs(mine, 2, 1000+1<<20)
	usageIs(files, 3, 1007+1<<20)

	if _, err := mine.Copy(t.Context(), "pending/b", "sent/b"); err != nil {
		t.Fatal(err)
	}
	usageIs(mine, 3, 1000+2<<20)
	if _, err := mine.Move(t.Context(), "pending/b", "sent/c"); err != nil {
		t.Fatal(err)
	}
	usageIs(mine, 3, 1000+2<<20)
	if err := mine.Delete(t.Context(), "sent/c"); err != nil {
		t.Fatal(err)
	}
	usageIs(mine, 2, 1000+1<<20)
	s.clock.advance(time.Hour)
	usageIs(mine, 1, 1<<20)
	if err := mine.Clear(t.Context()); err != nil {
		t.Fatal(err)
	}
	usageIs(mine, 0, 0)
	usageIs(files, 1, 7)
}

// an empty bucket and a folder of none are not an error, and a page past the
// bound is refused
func TestScanOfNothingAndPastItsBound(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	page, err := media.Scan(t.Context(), Query{})
	if err != nil || len(page.Objects) != 0 || page.More {
		t.Fatalf("an empty bucket: %+v, %v", page, err)
	}
	if _, err = media.Scan(t.Context(), Query{Limit: maxScanLimit + 1}); err == nil {
		t.Fatal("a page past its bound was read")
	}
	for i := range 150 {
		mustPut(t, media, fmt.Sprintf("k%03d", i), nil)
	}
	page, err = media.Scan(t.Context(), Query{})
	if err != nil || len(page.Objects) != scanLimit || !page.More || page.Next.After != "k099" {
		t.Fatalf("a page of the default limit: %d objects, more %v, next %+v, %v", len(page.Objects), page.More,
			page.Next, err)
	}
}
