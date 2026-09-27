package blobs

import (
	"bytes"
	"errors"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// a Copy shares the bytes: one file for both keys, which outlives the source
// and goes with the last key that names it
func TestACopySharesTheBytesAndOutlivesItsSource(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	for _, size := range []int{100, 1 << 20} {
		data := randomBytes(1, size)
		source := mustPut(t, media, "a", data, ContentType("video/mp4"), Meta("name", "a.mp4"))
		copied, err := media.Copy(t.Context(), "a", "b")
		if err != nil || copied.Key != "b" || copied.ETag != source.ETag || copied.ContentType != "video/mp4" ||
			copied.Meta["name"] != "a.mp4" {
			t.Fatalf("a Copy of %d bytes: %+v, %v", size, copied, err)
		}
		renamed, err := media.Copy(t.Context(), "b", "c", ContentType("video/webm"), Meta("name", "c.webm"))
		if err != nil || renamed.ContentType != "video/webm" || renamed.Meta["name"] != "c.webm" {
			t.Fatalf("a Copy that names its own type and meta: %+v, %v", renamed, err)
		}
		if files := len(s.filesIn(t, objectsDir)); files != fileCount(size) {
			t.Fatalf("three keys of %d bytes hold %d files", size, files)
		}
		for _, key := range []string{"a", "b"} {
			if err = media.Delete(t.Context(), key); err != nil {
				t.Fatal(err)
			}
			mustHold(t, media, "c", data)
		}
		if err = media.Delete(t.Context(), "c"); err != nil {
			t.Fatal(err)
		}
		s.mustHoldFiles(t, 0)
	}
	if _, err := media.Copy(t.Context(), "gone", "d"); !errors.Is(err, tinystore.ErrConflict) {
		t.Fatalf("a Copy from an absent key: %v", err)
	}
}

func fileCount(size int) int {
	if size > inlineSize {
		return 1
	}
	return 0
}

// a Move is one write: the destination written, the source gone, the bytes
// untouched; onto its own key it changes what a Copy onto it changes
func TestAMoveIsOneWrite(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	files := openTestBucket(t, s, "chat-files")
	mine := files.Of("users", 7)
	data := randomBytes(2, 1<<20)
	mustPut(t, mine, "pending/u1", data, TTL(24*time.Hour), Meta("name", "cat.jpg"))
	sent, err := mine.Move(t.Context(), "pending/u1", "sent/u1")
	if err != nil || sent.Key != "sent/u1" || sent.Meta["name"] != "cat.jpg" || !sent.Expires.IsZero() {
		t.Fatalf("a Move out of pending/: %+v, %v", sent, err)
	}
	mustBeAbsent(t, mine, "pending/u1")
	mustHold(t, mine, "sent/u1", data)
	s.mustHoldFiles(t, 1)

	moved, err := mine.Move(t.Context(), "sent/u1", "sent/u1", ContentType("image/jpeg"))
	if err != nil || moved.ContentType != "image/jpeg" {
		t.Fatalf("a Move onto its own key: %+v, %v", moved, err)
	}
	mustHold(t, mine, "sent/u1", data)
	mustPut(t, mine, "other", []byte("x"))
	if _, err = mine.Move(t.Context(), "sent/u1", "other", IfNoneMatch()); !errors.Is(err, tinystore.ErrConflict) {
		t.Fatalf("a Move onto a live key with IfNoneMatch: %v", err)
	}
	mustHold(t, mine, "sent/u1", data)
	if _, err = mine.Move(t.Context(), "nothing", "else"); !errors.Is(err, tinystore.ErrConflict) {
		t.Fatalf("a Move from an absent key: %v", err)
	}
}

// every write gives its object the expiry a Put would: its own, else the
// bucket's default from now, else none; a Copy or Move keeps no expiry of its source
func TestACopyOrMoveTakesTheExpiryOfAWrite(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	exports := openTestBucket(t, s, "exports", DefaultTTL(24*time.Hour))
	kept := openTestBucket(t, s, "kept")
	day := testStart.Add(24 * time.Hour)

	first := mustPut(t, exports, "a.zip", randomBytes(3, 100))
	if !first.Expires.Equal(day) {
		t.Fatalf("a Put in a bucket of a day expires at %v", first.Expires)
	}
	s.clock.advance(time.Hour)
	again := mustPut(t, exports, "a.zip", randomBytes(3, 100))
	copied, err := exports.Copy(t.Context(), "a.zip", "b.zip")
	if err != nil || !again.Expires.Equal(day.Add(time.Hour)) || !copied.Expires.Equal(day.Add(time.Hour)) {
		t.Fatalf("written again an hour later: %v, copied: %v, %v", again.Expires, copied.Expires, err)
	}
	moved, err := exports.Move(t.Context(), "b.zip", "c.zip", TTL(time.Minute))
	if err != nil || !moved.Expires.Equal(testStart.Add(time.Hour+time.Minute)) {
		t.Fatalf("a Move with its own TTL: %v, %v", moved.Expires, err)
	}
	mustPut(t, kept, "pending", randomBytes(4, 100), TTL(time.Hour))
	sent, err := kept.Move(t.Context(), "pending", "sent")
	if err != nil || !sent.Expires.IsZero() {
		t.Fatalf("a Move in a bucket without a default: %v, %v", sent.Expires, err)
	}
	at := testStart.Add(48 * time.Hour)
	fixed, err := kept.Copy(t.Context(), "sent", "fixed", ExpireAt(at))
	if err != nil || !fixed.Expires.Equal(at) {
		t.Fatalf("a Copy with ExpireAt: %v, %v", fixed.Expires, err)
	}
}

// an expired object is absent to every call: reads and Scan and Usage do
// not see it, IfNoneMatch claims its key, IfMatch conflicts with it, a Copy
// finds nothing; maintenance then removes its row and its bytes
func TestAnExpiredObjectIsAbsentToEveryOperation(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	for _, size := range []int{100, 1 << 20} {
		key := "expiring"
		object := mustPut(t, media, key, randomBytes(5, size), TTL(time.Minute))
		s.clock.advance(time.Minute)
		mustBeAbsent(t, media, key)
		page, err := media.Scan(t.Context(), Query{})
		usage, usageErr := media.Usage(t.Context())
		if err != nil || usageErr != nil || len(page.Objects) != 0 || usage != (Usage{}) {
			t.Fatalf("an expired object in a Scan: %+v, in Usage: %+v, %v, %v", page, usage, err, usageErr)
		}
		if _, err = media.Copy(t.Context(), key, "copy"); !errors.Is(err, tinystore.ErrConflict) {
			t.Fatalf("a Copy of an expired object: %v", err)
		}
		if err = media.Delete(t.Context(), key, IfMatch(object.ETag)); !errors.Is(err, tinystore.ErrConflict) {
			t.Fatalf("IfMatch against an expired object: %v", err)
		}
		mustPut(t, media, key, randomBytes(6, size), IfNoneMatch(), TTL(time.Minute))
		s.clock.advance(time.Minute)
		if done := s.maintain(t); done.Expired != 1 {
			t.Fatalf("maintenance removed %d expired objects", done.Expired)
		}
		s.mustHoldFiles(t, 0)
	}
}

// Delete removes a key, an absent one included, and with IfMatch only the
// version it names
func TestDeleteRemovesAKeyAndIfMatchOnlyItsVersion(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	if err := media.Delete(t.Context(), "never"); err != nil {
		t.Fatalf("a Delete of an absent key: %v", err)
	}
	if err := media.Delete(t.Context(), "never", IfMatch(`"a"`)); !errors.Is(err, tinystore.ErrConflict) {
		t.Fatalf("a conditional Delete of an absent key: %v", err)
	}
	old := mustPut(t, media, "doc", randomBytes(7, 1<<20))
	mustPut(t, media, "doc", randomBytes(8, 1<<20))
	if err := media.Delete(t.Context(), "doc", IfMatch(old.ETag)); !errors.Is(err, tinystore.ErrConflict) {
		t.Fatalf("a Delete of a version replaced since: %v", err)
	}
	now, _, _ := media.Stat(t.Context(), "doc")
	if err := media.Delete(t.Context(), "doc", IfMatch(now.ETag)); err != nil {
		t.Fatal(err)
	}
	mustBeAbsent(t, media, "doc")
	s.mustHoldFiles(t, 0)
	if err := media.Delete(t.Context(), "doc", ContentType("x")); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a Delete with an option it does not take: %v", err)
	}
	if _, err := media.Copy(t.Context(), "a", "b", Size(1)); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a Copy with Size: %v", err)
	}
	_, err := media.Put(t.Context(), "doc", bytes.NewReader(nil), IfMatch(`"a"`), IfNoneMatch())
	if !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("both conditions: %v", err)
	}
}
