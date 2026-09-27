package blobs

import (
	"bytes"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// a key is its own bytes everywhere: it names no file, so keys that differ
// by case are two objects on NTFS and APFS too, and a name Windows reserves
// is a key like any other
func TestAKeyIsItsOwnBytesOnEveryFileSystem(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	keys := []string{
		"Photo.jpg", "photo.jpg", "PHOTO.JPG", "CON", "nul.txt", "a:b*c?d", "trailing. ", "héllo/wörld", "日本/語",
		"🙂", strings.Repeat("x", maxPath),
	}
	for i, key := range keys {
		mustPut(t, media, key, randomBytes(uint64(i), inlineSize+i))
	}
	for i, key := range keys {
		mustHold(t, media, key, randomBytes(uint64(i), inlineSize+i))
	}
	s.mustHoldFiles(t, len(keys)-1)
}

// a path that is not one is refused with ErrInvalid naming it, and so is an
// owner that is not a segment
func TestAPathThatIsNotOneIsRefused(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	for _, key := range []string{
		"", "/a", "a/", "a//b", ".", "..", "a/./b", "a/../b", "\xff", "a\x00b", "line\nbreak", "tab\t",
		strings.Repeat("a/", maxSegments) + "a", strings.Repeat("x", maxPath+1),
	} {
		_, err := media.Put(t.Context(), key, bytes.NewReader(nil))
		var named *KeyError
		if !errors.Is(err, tinystore.ErrInvalid) || !errors.As(err, &named) {
			t.Fatalf("a Put of %q: %v", key, err)
		}
		if _, _, err = media.Open(t.Context(), key); !errors.Is(err, tinystore.ErrInvalid) {
			t.Fatalf("an Open of %q: %v", key, err)
		}
	}
	for _, owner := range []any{"a/b", "", "..", 3.5, []byte("bytes")} {
		if _, err := media.Of(owner).Put(t.Context(), "k", bytes.NewReader(nil)); !errors.Is(err, tinystore.ErrInvalid) {
			t.Fatalf("an owner %v: %v", owner, err)
		}
	}
	for _, name := range []string{"", "Avatars", "a b", strings.Repeat("a", 65)} {
		if _, err := OpenBucket(t.Context(), s.Store, name); !errors.Is(err, tinystore.ErrInvalid) {
			t.Fatalf("a bucket named %q: %v", name, err)
		}
	}
	for _, option := range []Option{
		ContentType(strings.Repeat("t", maxType+1)), Meta("Name", "x"), Meta("n", strings.Repeat("v", maxMeta)),
		TTL(0), ExpireAt(time.Time{}), IfMatch(""),
	} {
		if _, err := media.Put(t.Context(), "k", bytes.NewReader(nil), option); !errors.Is(err, tinystore.ErrInvalid) {
			t.Fatalf("an option out of its bounds: %v", err)
		}
	}
}

// an owner is a segment: a string, or an integer by its decimal text, named
// or not, so that a handler holding an int64 and one holding a URL's text
// name one folder
func TestAnOwnerIsItsDecimalText(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	type userID int64
	mustPut(t, media.Of("users", 42), "a", []byte("a"))
	for _, owner := range []any{42, int64(42), "42", userID(42), uint8(42)} {
		mustHold(t, media.Of("users", owner), "a", []byte("a"))
	}
}

func TestPrefixEnd(t *testing.T) {
	for prefix, want := range map[string]string{
		"users/4/": "users/40", "2026-09-27/": "2026-09-270", "a": "b", "": "\xff", "é": "\xc3\xaa",
	} {
		if got := prefixEnd(prefix); got != want {
			t.Fatalf("prefixEnd(%q) = %q, want %q", prefix, got, want)
		}
	}
}

func TestMatches(t *testing.T) {
	etag := `"9f86d081884c7d659a2feaa0c55ad015"`
	for list, want := range map[string]bool{
		etag: true, `"a1", ` + etag: true, "*": true, "W/" + etag: false, `"a1"`: false,
		strings.Trim(etag, `"`): false,
	} {
		if got := matches(list, etag); got != want {
			t.Fatalf("matches(%q) = %v", list, got)
		}
	}
}
