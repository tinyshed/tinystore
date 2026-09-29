package server

import (
	"bytes"
	"context"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/server/internal/client"
	"github.com/tinyshed/tinystore/server/wire"
)

func openKV(t *testing.T, conn *client.Conn, bucket wire.KVBucket) uint64 {
	t.Helper()
	body, err := conn.Call(t.Context(), wire.KVOpen, bucket)
	if err != nil {
		t.Fatal(err)
	}
	var handle wire.Handle
	if err := handle.Decode(body); err != nil {
		t.Fatal(err)
	}
	return handle.Handle
}

func kvDo(t *testing.T, conn *client.Conn, method wire.Method, ask wire.KVCall) (wire.KVEntry, error) {
	t.Helper()
	body, err := conn.Call(t.Context(), method, ask)
	if err != nil {
		return wire.KVEntry{}, err
	}
	var entry wire.KVEntry
	return entry, entry.Decode(body)
}

func mustKV(t *testing.T, conn *client.Conn, method wire.Method, ask wire.KVCall) wire.KVEntry {
	t.Helper()
	entry, err := kvDo(t, conn, method, ask)
	if err != nil {
		t.Fatalf("%#04x %+v: %v", uint16(method), ask, err)
	}
	return entry
}

func text(s string) wire.KVValue {
	return wire.KVValue{Kind: wire.KVBytes, Bytes: []byte(s)}
}

func TestKVOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	sessions := openKV(t, conn, wire.KVBucket{Name: "sessions"})
	key := wire.KVCall{Handle: sessions, Owners: []string{"7"}, Key: "token"}

	set := key
	set.Value, set.TTL = text("hello"), time.Hour.Milliseconds()
	written := mustKV(t, conn, wire.KVSet, set)
	if !written.Found || len(written.Version) == 0 || written.Expires < time.Now().Add(59*time.Minute).UnixMilli() {
		t.Fatalf("set: %+v", written)
	}
	got := mustKV(t, conn, wire.KVGet, key)
	if !got.Found || string(got.Value.Bytes) != "hello" || !bytes.Equal(got.Version, written.Version) ||
		got.Expires != written.Expires {
		t.Fatalf("get: %+v", got)
	}
	if has := mustKV(t, conn, wire.KVHas, key); !has.Found {
		t.Fatal("has")
	}

	stale := set
	stale.IfVersion = []byte("zz")
	_, err := kvDo(t, conn, wire.KVSet, stale)
	var failure *wire.Error
	if code := codeOfError(err); code != wire.CodeConflict || !asError(err, &failure) ||
		failure.What["bucket"] != "sessions" || failure.What["key"] != "7/token" {
		t.Fatalf("a stale version: %v %+v", err, failure)
	}

	claim := set
	claim.IfAbsent, claim.Value = true, text("other")
	if there := mustKV(t, conn, wire.KVSet, claim); there.Found || string(there.Value.Bytes) != "hello" {
		t.Fatalf("set if absent over a live key: %+v", there)
	}

	if taken := mustKV(t, conn, wire.KVTake, key); !taken.Found || string(taken.Value.Bytes) != "hello" {
		t.Fatalf("take: %+v", taken)
	}
	if gone := mustKV(t, conn, wire.KVGet, key); gone.Found {
		t.Fatalf("a taken key: %+v", gone)
	}
	touch := key
	touch.TTL = time.Minute.Milliseconds()
	if touched := mustKV(t, conn, wire.KVTouch, touch); touched.Found {
		t.Fatal("touched an absent key")
	}
}

// the largest value a bucket keeps, 1 MiB, travels both ways in one body
func TestTheLargestValueTravelsInOneBody(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	large := openKV(t, conn, wire.KVBucket{Name: "large"})
	value := wire.KVValue{Kind: wire.KVBytes, Bytes: bytes.Repeat([]byte{7}, 1<<20)}

	if _, err := kvDo(t, conn, wire.KVSet, wire.KVCall{Handle: large, Key: "k", Value: value}); err != nil {
		t.Fatalf("set 1 MiB: %v", err)
	}
	got, err := kvDo(t, conn, wire.KVGet, wire.KVCall{Handle: large, Key: "k"})
	if err != nil || !bytes.Equal(got.Value.Bytes, value.Bytes) {
		t.Fatalf("read back %d bytes of %d: %v", len(got.Value.Bytes), len(value.Bytes), err)
	}
}

func asError(err error, target **wire.Error) bool {
	failure, ok := err.(*wire.Error) //nolint:errorlint // the client returns it as it is
	*target = failure
	return ok
}

// a key is its text: an integer is its decimal spelling, and a key of bytes
// that are not UTF-8 comes back as they were
func TestAKVKeyIsItsText(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	codes := openKV(t, conn, wire.KVBucket{Name: "codes"})

	m := wire.BeginMap(nil)
	m.Uint(1, codes)
	m.Int(3, 42)
	m.Key(4)
	m.SetBuf(wire.AppendInt(m.Buf(), 9))
	if _, err := conn.Call(t.Context(), wire.KVSet, rawBody(m.End())); err != nil {
		t.Fatal(err)
	}
	if got := mustKV(t, conn, wire.KVGet, wire.KVCall{Handle: codes, Key: "42"}); got.Value.Kind != wire.KVInt ||
		got.Value.Int != 9 {
		t.Fatalf("42 read as \"42\": %+v", got)
	}

	binary := "\xff\x00key"
	mustKV(t, conn, wire.KVSet, wire.KVCall{Handle: codes, Owners: []string{"raw"}, Key: binary, Value: text("x")})
	page := scanAll(t, conn, wire.KVCall{Handle: codes, Owners: []string{"raw"}})
	if len(page) != 1 || page[0].Key != binary {
		t.Fatalf("a key of bytes scanned as %+v", page)
	}
}

func scanAll(t *testing.T, conn *client.Conn, ask wire.KVCall) []wire.KVEntry {
	t.Helper()
	var entries []wire.KVEntry
	for {
		page, next := scanPage(t, conn, ask)
		entries = append(entries, page...)
		if !next.More {
			return entries
		}
		ask.After = next.After
	}
}

func scanPage(t *testing.T, conn *client.Conn, ask wire.KVCall) ([]wire.KVEntry, wire.KVPage) {
	t.Helper()
	st, err := conn.Open(t.Context(), wire.KVScan, ask, true)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := st.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	var entries []wire.KVEntry
	for {
		body, last, err := st.Next(t.Context())
		if err != nil {
			t.Fatal(err)
		}
		if last {
			var page wire.KVPage
			if err := page.Decode(body); err != nil {
				t.Fatal(err)
			}
			return entries, page
		}
		var entry wire.KVEntry
		if err := entry.Decode(body); err != nil {
			t.Fatal(err)
		}
		entries = append(entries, entry)
	}
}

func TestAKVScanPagesABranchAndAClearEmptiesIt(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	drafts := openKV(t, conn, wire.KVBucket{Name: "drafts"})
	for _, key := range []string{"a", "b", "c", "d", "e"} {
		mustKV(t, conn, wire.KVSet, wire.KVCall{Handle: drafts, Owners: []string{"u1"}, Key: key, Value: text(key)})
	}

	first, next := scanPage(t, conn, wire.KVCall{Handle: drafts, Owners: []string{"u1"}, Limit: 2})
	if len(first) != 2 || first[0].Key != "a" || string(first[1].Value.Bytes) != "b" || !next.More ||
		next.After != "b" {
		t.Fatalf("the first page: %+v %+v", first, next)
	}
	if all := scanAll(t, conn, wire.KVCall{Handle: drafts, Owners: []string{"u1"}, Limit: 2}); len(all) != 5 {
		t.Fatalf("%d keys paged", len(all))
	}

	mustKV(t, conn, wire.KVClear, wire.KVCall{Handle: drafts, Owners: []string{"u1"}})
	if left := scanAll(t, conn, wire.KVCall{Handle: drafts, Owners: []string{"u1"}}); len(left) != 0 {
		t.Fatalf("%d keys after the clear", len(left))
	}
}

func TestKVCountersOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	attempts := openKV(t, conn, wire.KVBucket{
		Name: "attempts", Counters: true, LoseAtMost: 1000,
		DefaultTTL: time.Minute.Milliseconds(),
	})
	ip := wire.KVCall{Handle: attempts, Owners: []string{"ip"}, Key: "10.0.0.1"}

	for want := int64(1); want <= 3; want++ {
		add := ip
		add.N = 1
		if got := mustKV(t, conn, wire.KVAdd, add); got.Value.Int != want {
			t.Fatalf("add: %+v, want %d", got, want)
		}
	}
	higher := ip
	higher.N = 10
	if got := mustKV(t, conn, wire.KVMax, higher); got.Value.Int != 10 {
		t.Fatalf("max: %+v", got)
	}
	if got := mustKV(t, conn, wire.KVGet, ip); got.Value.Kind != wire.KVInt || got.Value.Int != 10 {
		t.Fatalf("get: %+v", got)
	}
	mustKV(t, conn, wire.KVDelete, ip)
	if got := mustKV(t, conn, wire.KVGet, ip); got.Value.Int != 0 {
		t.Fatalf("a deleted counter: %+v", got)
	}
	if _, err := kvDo(t, conn, wire.KVSet, ip); codeOfError(err) != wire.CodeInvalid {
		t.Fatalf("a set on counters: %v", err)
	}
}

// a batch is one transaction: a call that fails rolls back the calls before it
func TestAKVBatchRollsBackWhenOneOfItsCallsFails(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	accounts := openKV(t, conn, wire.KVBucket{Name: "accounts"})
	seen := openKV(t, conn, wire.KVBucket{Name: "seen"})

	batch := wire.KVCalls{Calls: []wire.KVOperation{
		{Method: wire.KVSet, KVCall: wire.KVCall{Handle: accounts, Key: "a", Value: text("1")}},
		{Method: wire.KVSet, KVCall: wire.KVCall{Handle: seen, Key: "evt", Value: text("1"), IfVersion: []byte("zz")}},
	}}
	_, err := conn.Call(t.Context(), wire.KVBatch, batch)
	var failure *wire.Error
	if !asError(err, &failure) || failure.Code != wire.CodeConflict || failure.What["call"] != "1" ||
		failure.What["bucket"] != "seen" {
		t.Fatalf("a batch whose second call conflicts: %v %+v", err, failure)
	}
	if got := mustKV(t, conn, wire.KVGet, wire.KVCall{Handle: accounts, Key: "a"}); got.Found {
		t.Fatal("the first call of a failed batch stayed")
	}

	batch.Calls[1].IfVersion = nil
	body, err := conn.Call(t.Context(), wire.KVBatch, batch)
	if err != nil {
		t.Fatal(err)
	}
	var results wire.KVResults
	if decodeErr := results.Decode(body); decodeErr != nil || len(results.Entries) != 2 || !results.Entries[1].Found {
		t.Fatalf("a batch's results: %+v, %v", results, decodeErr)
	}

	view := wire.KVCalls{Calls: []wire.KVOperation{
		{Method: wire.KVGet, KVCall: wire.KVCall{Handle: accounts, Key: "a"}},
		{Method: wire.KVHas, KVCall: wire.KVCall{Handle: seen, Key: "evt"}},
	}}
	body, err = conn.Call(t.Context(), wire.KVView, view)
	results = wire.KVResults{}
	if err != nil || results.Decode(body) != nil || string(results.Entries[0].Value.Bytes) != "1" ||
		!results.Entries[1].Found {
		t.Fatalf("a view: %+v, %v", results, err)
	}
	view.Calls[1].Method = wire.KVDelete
	if _, err := conn.Call(t.Context(), wire.KVView, view); codeOfError(err) != wire.CodeInvalid {
		t.Fatalf("a write in a view: %v", err)
	}
}

// a program in Go and a client over the wire read each other's buckets: a
// value is what its row keeps
func TestAWireClientAndAGoProgramReadEachOthersBuckets(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	ctx := context.Background()
	state, err := ts.server.kvStore(ctx)
	if err != nil {
		t.Fatal(err)
	}
	names, err := kv.OpenBucket[string](ctx, state, "shared")
	if err != nil {
		t.Fatal(err)
	}
	counts, err := kv.OpenBucket[int64](ctx, state, "shared")
	if err != nil {
		t.Fatal(err)
	}
	shared := openKV(t, conn, wire.KVBucket{Name: "shared"})

	if err := names.Set(ctx, "name", "Ada"); err != nil {
		t.Fatal(err)
	}
	if got := mustKV(t, conn, wire.KVGet, wire.KVCall{Handle: shared, Key: "name"}); string(got.Value.Bytes) != "Ada" {
		t.Fatalf("Go's string over the wire: %+v", got)
	}
	mustKV(t, conn, wire.KVSet, wire.KVCall{Handle: shared, Key: "count", Value: wire.KVValue{Kind: wire.KVInt, Int: 41}})
	if n, found, err := counts.Get(ctx, "count"); n != 41 || !found || err != nil {
		t.Fatalf("the wire's integer in Go: %d %v %v", n, found, err)
	}
}
