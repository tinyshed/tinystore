package kv

import (
	"bytes"
	"errors"
	"strings"
	"testing"

	"github.com/tinyshed/tinystore"
)

type userID int64

type label string

// an integer key is its decimal text, so a handler holding an int64 and one
// holding a URL's text name the same key; anything else is refused
func TestAnIntegerKeyIsItsDecimalText(t *testing.T) {
	for _, spelling := range []any{"42", 42, int64(42), int32(42), uint8(42), uint64(42), userID(42), []byte("42")} {
		text, err := keyText(spelling)
		if err != nil || text != "42" {
			t.Fatalf("%T %v is %q, %v; want 42", spelling, spelling, text, err)
		}
	}
	if text, err := keyText(label("iPhone")); err != nil || text != "iPhone" {
		t.Fatalf("a named string is %q, %v", text, err)
	}
	if text, err := keyText(-7); err != nil || text != "-7" {
		t.Fatalf("-7 is %q, %v", text, err)
	}
	for _, refused := range []any{"", []byte{}, 4.2, struct{}{}, nil, true} {
		if _, err := keyText(refused); !errors.Is(err, tinystore.ErrInvalid) {
			t.Fatalf("%T %v is a key: %v", refused, refused, err)
		}
	}

	state := openTestState(t, t.TempDir())
	sessions := openTestBucket[string](t, state, "sessions")
	if err := sessions.Of(int64(7)).Set(t.Context(), int64(42), "iPhone"); err != nil {
		t.Fatal(err)
	}
	if value, found, err := sessions.Of("7").Get(t.Context(), "42"); err != nil || !found || value != "iPhone" {
		t.Fatalf("the key written as integers read as text: %q, %v, %v", value, found, err)
	}
}

// a path is owners and a key after their marks, a 00 inside a name escaped, so
// that a branch is one range and no two names make the same path
//
//	Of("tenant-7", 42), "iPhone" → 01 tenant-7 00 · 01 42 00 · 02 iPhone
func TestAPathKeepsItsNamesApart(t *testing.T) {
	path := appendKey(appendOwner(appendOwner(nil, "tenant-7"), "42"), "iPhone")
	want := []byte("\x01tenant-7\x00\x0142\x00\x02iPhone")
	if !bytes.Equal(path, want) {
		t.Fatalf("path %q, want %q", path, want)
	}
	if key := keyOf(appendKey(appendOwner(nil, "a"), "b\x00c"), len(appendOwner(nil, "a"))); key != "b\x00c" {
		t.Fatalf("a key with a 00 read back as %q", key)
	}

	paths := map[string]string{}
	for shown, path := range map[string][]byte{
		`Of("a", "b") "c"`: appendKey(appendOwner(appendOwner(nil, "a"), "b"), "c"),
		`Of("a/b") "c"`:    appendKey(appendOwner(nil, "a/b"), "c"),
		`Of("a\x00b") "c"`: appendKey(appendOwner(nil, "a\x00b"), "c"),
		`Of("a") "b/c"`:    appendKey(appendOwner(nil, "a"), "b/c"),
		`Of("a") "b\x00c"`: appendKey(appendOwner(nil, "a"), "b\x00c"),
		`"a/b/c"`:          appendKey(nil, "a/b/c"),
	} {
		if other, taken := paths[string(path)]; taken {
			t.Fatalf("%s and %s make one path", shown, other)
		}
		paths[string(path)] = shown
	}
}

func TestAKeyPathOverOneKibibyteIsRefused(t *testing.T) {
	state := openTestState(t, t.TempDir())
	bucket := openTestBucket[string](t, state, "long")
	long := strings.Repeat("k", maxPath)
	err := bucket.Set(t.Context(), long, "v")
	var named *KeyError
	if !errors.Is(err, tinystore.ErrInvalid) || !errors.As(err, &named) || named.Bucket != "long" {
		t.Fatalf("a path over 1 KiB: %v", err)
	}
	if err = bucket.Of(struct{}{}).Set(t.Context(), "k", "v"); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("an owner that is not text: %v", err)
	}
}
