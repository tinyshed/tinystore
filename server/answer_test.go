package server

import (
	"bytes"
	"context"
	"strconv"
	"testing"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/server/wire"
)

// a download holds the store's memory for its answer from the engine's return
// until its last DATA has left, since a client takes the DATA at its own pace
func TestADownloadHoldsTheStoresMemoryUntilItsLastData(t *testing.T) {
	root := t.TempDir()
	store, err := tinystore.Open(context.Background(), root, tinystore.Options{Manual: true, Memory: 16 << 20})
	if err != nil {
		t.Fatal(err)
	}
	ts := serveTestStore(t, root, store, Options{})
	conn := ts.dial(t, wire.Hello{})
	bucket := openKV(t, conn, wire.KVBucket{Name: "big"})
	value := wire.KVValue{Kind: wire.KVBytes, Bytes: bytes.Repeat([]byte("x"), 64<<10)}
	const keys = 40
	for i := range keys {
		mustKV(t, conn, wire.KVSet, wire.KVCall{Handle: bucket, Key: strconv.Itoa(i), Value: value})
	}

	st, err := conn.Open(t.Context(), wire.KVScan, wire.KVCall{Handle: bucket, Limit: keys}, true)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = st.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	if used := store.Memory().Used; used < keys*64<<10 {
		t.Fatalf("the store holds %d bytes for an answer of %d the client has not taken", used, keys*64<<10)
	}
	for last := false; !last; {
		if _, last, err = st.Next(t.Context()); err != nil {
			t.Fatal(err)
		}
	}
	waitFor(t, "the answer let go once its last DATA left", func() bool { return store.Memory().Used == 0 })
}
