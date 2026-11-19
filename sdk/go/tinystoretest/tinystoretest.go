// Package tinystoretest opens a store for a test, as net/http/httptest
// starts a server for one: a directory of the test's own, the core in this
// process, closed and removed when the test ends.
//
//	func TestReminder(t *testing.T) {
//		store := tinystoretest.Open(t, tinystore.Clock(time.Date(2026, 11, 19, 9, 0, 0, 0, time.UTC)))
//		err := store.Clock().Advance(ctx, time.Hour)
//	}
package tinystoretest

import (
	"context"
	"testing"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/temp"
)

// Open opens a store in a directory of t's own, which t.TempDir removes
// once t.Cleanup has closed the store (contract STORE-12); a store that does
// not open fails the test with t.Fatal. It opens embedded, the core in this
// process, unless tinystore.Private asks for a child; every option of
// tinystore.Open passes through, tinystore.Clock among them.
func Open(t testing.TB, opts ...tinystore.Option) *tinystore.Store {
	t.Helper()
	dir := t.TempDir()
	given := append([]tinystore.Option{temp.EmbeddedUnlessPrivate.(tinystore.Option)}, opts...)
	store, err := tinystore.Open(context.Background(), dir, given...)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := store.Close(); err != nil {
			t.Error(err)
		}
	})
	return store
}
