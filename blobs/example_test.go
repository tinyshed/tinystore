package blobs_test

import (
	"archive/zip"
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/blobs"
)

// clock is the store's clock in the examples, moved rather than waited for
type clock struct{ now time.Time }

func (c *clock) Now() time.Time { return c.now }

func (c *clock) advance(d time.Duration) { c.now = c.now.Add(d) }

// exampleObjects opens blobs in a Manual store in a new directory, so that an
// example moves its clock instead of sleeping; done closes and removes it
func exampleObjects() (objects *blobs.Store, at *clock, done func()) {
	ctx := context.Background()
	directory, err := os.MkdirTemp("", "tinystore-blobs-")
	check(err)
	at = &clock{now: time.Date(2026, 9, 27, 12, 0, 0, 0, time.UTC)}
	store, err := tinystore.Open(ctx, directory, tinystore.Options{Manual: true, Clock: at.Now})
	check(err)
	objects, err = blobs.Open(ctx, store, blobs.Options{})
	check(err)
	return objects, at, func() {
		_ = store.Close(ctx)
		_ = os.RemoveAll(directory)
	}
}

// check ends an example that meets an error
func check(err error) {
	if err != nil {
		panic(err)
	}
}

// An avatar's original and the sizes a page shows live in the user's folder. A
// job makes the sizes from whatever original is there when it runs, and
// deleting the account clears the folder.
func Example_avatars() {
	ctx := context.Background()
	objects, _, done := exampleObjects()
	defer done()
	avatars, err := blobs.OpenBucket(ctx, objects, "avatars", blobs.MaxSize(20<<20))
	check(err)

	mine := avatars.Of(42)
	original, err := mine.Put(ctx, "original", bytes.NewReader(make([]byte, 300_000)),
		blobs.ContentType("image/jpeg"))
	check(err)
	for _, side := range []int{256, 64} { // what the resizing job writes
		sized, createErr := mine.Create(ctx, fmt.Sprintf("%d.jpg", side), blobs.ContentType("image/jpeg"))
		check(createErr)
		_, createErr = sized.Write(make([]byte, side*side/4))
		check(createErr)
		_, createErr = sized.Commit(ctx)
		check(createErr)
		sized.Abort() // after Commit it does nothing
	}
	for object, err := range mine.All(ctx, blobs.Query{}) {
		check(err)
		fmt.Println(object.Key, object.Size, object.ContentType)
	}
	_, err = mine.Put(ctx, "huge", bytes.NewReader(make([]byte, 21<<20)))
	fmt.Println(errors.Is(err, tinystore.ErrLimit), original.Size)

	check(mine.Clear(ctx))
	_, found, err := mine.Stat(ctx, "original")
	check(err)
	fmt.Println(found)
	// Output:
	// 256.jpg 16384 image/jpeg
	// 64.jpg 1024 image/jpeg
	// original 300000 image/jpeg
	// true 300000
	// false
}

// A file the composer uploads waits a day under pending/. Sending moves it to
// sent/, where it stays, and the user's folder answers a quota check by
// counting its objects' rows.
func Example_chatAttachments() {
	ctx := context.Background()
	objects, clock, done := exampleObjects()
	defer done()
	files, err := blobs.OpenBucket(ctx, objects, "chat-files")
	check(err)

	mine := files.Of("users", 7)
	for _, id := range []string{"u1", "u2"} {
		_, err = mine.Put(ctx, "pending/"+id, strings.NewReader("a photo of "+id), blobs.TTL(24*time.Hour),
			blobs.ContentType("image/jpeg"), blobs.Meta("name", id+".jpg"))
		check(err)
	}
	sent, err := mine.Move(ctx, "pending/u1", "sent/u1") // sending: no expiry in a bucket without a default
	check(err)
	fmt.Println(sent.Key, sent.Meta["name"], sent.Expires.IsZero())

	clock.advance(25 * time.Hour) // u2 was never sent
	usage, err := mine.Usage(ctx)
	check(err)
	fmt.Println(usage.Objects, usage.Bytes)
	page, err := mine.Scan(ctx, blobs.Query{Prefix: "sent/", Limit: 50}) // "your files"
	check(err)
	fmt.Println(len(page.Objects), page.Objects[0].Key)
	// Output:
	// sent/u1 u1.jpg true
	// 1 13
	// 1 sent/u1
}

// A camera writes one object a minute, so a crash loses the minute being
// written and never the minutes before it. A player seeks, and the server
// answers its range with http.ServeContent.
func Example_recordings() {
	ctx := context.Background()
	objects, _, done := exampleObjects()
	defer done()
	recordings, err := blobs.OpenBucket(ctx, objects, "recordings", blobs.DefaultTTL(30*24*time.Hour))
	check(err)

	start := time.Date(2026, 9, 27, 9, 30, 0, 0, time.UTC)
	for minute := range 3 {
		name := start.Add(time.Duration(minute)*time.Minute).Format("2006-01-02/15-04") + ".mp4"
		segment, createErr := recordings.Of("cam-1").Create(ctx, name, blobs.ContentType("video/mp4"))
		check(createErr)
		_, createErr = io.Copy(segment, strings.NewReader(strings.Repeat(fmt.Sprint(minute), 100_000)))
		check(createErr)
		_, createErr = segment.Commit(ctx)
		check(createErr)
	}
	day, err := recordings.Of("cam-1").Scan(ctx, blobs.Query{Prefix: "2026-09-27/"})
	check(err)
	fmt.Println(len(day.Objects), day.Objects[2].Key)

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		video, found, openErr := recordings.Of("cam-1").Open(r.Context(), strings.TrimPrefix(r.URL.Path, "/"))
		if openErr != nil || !found {
			http.NotFound(w, r)
			return
		}
		defer video.Close()
		w.Header().Set("ETag", video.ETag)
		w.Header().Set("Content-Type", video.ContentType)
		http.ServeContent(w, r, "", video.Modified, video)
	}))
	defer server.Close()
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, server.URL+"/2026-09-27/09-32.mp4", nil)
	check(err)
	request.Header.Set("Range", "bytes=99990-99999")
	response, err := http.DefaultClient.Do(request)
	check(err)
	defer response.Body.Close()
	tail, err := io.ReadAll(response.Body)
	check(err)
	fmt.Println(response.StatusCode, string(tail))
	// Output:
	// 3 2026-09-27/09-32.mp4
	// 206 2222222222
}

// A job streams a zip into an upload; the link works for a day, and the
// bytes leave with the object.
func Example_export() {
	ctx := context.Background()
	objects, clock, done := exampleObjects()
	defer done()
	exports, err := blobs.OpenBucket(ctx, objects, "exports", blobs.DefaultTTL(24*time.Hour))
	check(err)

	out, err := exports.Of(42).Create(ctx, "e1.zip", blobs.ContentType("application/zip"),
		blobs.Meta("name", "notes-2026-09-27.zip"))
	check(err)
	defer out.Abort()
	archive := zip.NewWriter(out)
	note, err := archive.Create("note-1.txt")
	check(err)
	_, err = note.Write([]byte("milk, bread, eggs, tea"))
	check(err)
	check(archive.Close())
	export, err := out.Commit(ctx)
	check(err)
	fmt.Println(export.Meta["name"], export.Expires.Sub(export.Modified))

	clock.advance(25 * time.Hour)
	_, found, err := exports.Of(42).Open(ctx, "e1.zip")
	check(err)
	maintained, err := objects.Maintain(ctx)
	check(err)
	fmt.Println(found, maintained.Expired)
	// Output:
	// notes-2026-09-27.zip 24h0m0s
	// false 1
}

// A client that retries an upload sends If-None-Match: *, and the retry adds
// nothing. An editor saving sends the ETag it read, and a save over someone
// else's is refused.
func Example_documents() {
	ctx := context.Background()
	objects, _, done := exampleObjects()
	defer done()
	documents, err := blobs.OpenBucket(ctx, objects, "documents")
	check(err)

	first, err := documents.Put(ctx, "plans/q4.md", strings.NewReader("# Q4"), blobs.IfNoneMatch())
	check(err)
	_, err = documents.Put(ctx, "plans/q4.md", strings.NewReader("# Q4"), blobs.IfNoneMatch()) // the retry
	fmt.Println(errors.Is(err, tinystore.ErrConflict))

	mine, err := documents.Put(ctx, "plans/q4.md", strings.NewReader("# Q4\nship blobs"), blobs.IfMatch(first.ETag))
	check(err)
	_, err = documents.Put(ctx, "plans/q4.md", strings.NewReader("# Q4\nstale"), blobs.IfMatch(first.ETag))
	fmt.Println(errors.Is(err, tinystore.ErrConflict), mine.ETag != first.ETag)
	// Output:
	// true
	// true true
}
