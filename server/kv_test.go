package server

import (
	"bytes"
	"context"
	"errors"
	"slices"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/server/internal/client"
	"github.com/tinyshed/tinystore/server/wire"
)

func TestACancelledPointReadLetsTheConnectionGoOn(t *testing.T) {
	root := t.TempDir()
	store, err := tinystore.Open(t.Context(), root, tinystore.Options{Manual: true, Memory: 16 << 20})
	if err != nil {
		t.Fatal(err)
	}
	ts := serveTestStore(t, root, store, Options{})
	entered := make(chan struct{})
	get := ts.server.methods[wire.KVGet]
	ts.server.methods[wire.KVGet] = func(c *call) error {
		close(entered)
		return get(c)
	}
	conn := ts.dial(t, wire.Hello{})
	bucket := openKV(t, conn, wire.KVBucket{Name: "notes"})
	waitFor(t, "the open call released its memory", func() bool { return store.Memory().Used == 0 })

	held, err := store.Reserve(t.Context(), store.Memory().Capacity)
	if err != nil {
		t.Fatal(err)
	}
	defer held.Release()
	ctx, cancel := context.WithTimeout(t.Context(), time.Second)
	defer cancel()
	st, err := conn.Open(ctx, wire.KVGet, wire.KVCall{Handle: bucket, Key: "one"}, true)
	if err != nil {
		t.Fatal(err)
	}
	select {
	case <-entered:
	case <-ctx.Done():
		t.Fatal("the point read did not start")
	}
	if err = st.Cancel(); err != nil {
		t.Fatal(err)
	}

	_, err = st.Response(ctx)
	failure, ok := errors.AsType[*wire.Error](err)
	if !ok || failure.Code != wire.CodeCancelled {
		t.Fatalf("a point read cancelled while memory was held: %v", err)
	}
	held.Release()
	ts.server.methods[wire.KVGet] = get
	mustKV(t, conn, wire.KVGet, wire.KVCall{Handle: bucket, Key: "one"})
}

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

func allow(t *testing.T, conn *client.Conn, ask wire.KVCall) wire.KVAllowance {
	t.Helper()
	body, err := conn.Call(t.Context(), wire.KVAllow, ask)
	if err != nil {
		t.Fatal(err)
	}
	var allowance wire.KVAllowance
	if err = allowance.Decode(body); err != nil {
		t.Fatal(err)
	}
	return allowance
}

// a limiter over the wire lets its burst through, then says how long to wait,
// each key and each branch on its own
func TestALimiterOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	limiter := openKV(t, conn, wire.KVBucket{Name: "api", Rate: 2, Per: time.Minute.Milliseconds()})
	key := wire.KVCall{Handle: limiter, Owners: []string{"tenant-7"}, Key: "user-1"}

	if got := allow(t, conn, key); !got.OK || got.Left != 1 {
		t.Fatalf("the first: %+v", got)
	}
	if got := allow(t, conn, key); !got.OK || got.Left != 0 {
		t.Fatalf("the second: %+v", got)
	}
	if got := allow(t, conn, key); got.OK || got.RetryAfter < 29_000 || got.RetryAfter > 30_000 {
		t.Fatalf("past the burst: %+v; want to wait about 30 s", got)
	}
	other := key
	other.Owners = nil
	if got := allow(t, conn, other); !got.OK {
		t.Fatalf("the same key in another branch: %+v", got)
	}

	many := key
	many.Key, many.N = "user-2", 3
	if _, err := conn.Call(t.Context(), wire.KVAllow, many); codeOfError(err) != wire.CodeInvalid {
		t.Fatalf("three past a burst of two: %v", err)
	}
	if _, err := kvDo(t, conn, wire.KVGet, key); codeOfError(err) != wire.CodeInvalid {
		t.Fatalf("a get of a limiter: %v", err)
	}
	if _, err := conn.Call(t.Context(), wire.KVOpen, wire.KVBucket{Name: "both", Counters: true, Rate: 1, Per: 1000}); codeOfError(err) != wire.CodeInvalid {
		t.Fatalf("counters and a limiter at once: %v", err)
	}
}

// a config changed through one connection reaches a watcher on another at
// once: the kept fields first, then after each change, until it cancels
func TestAConfigChangeReachesEveryWatcher(t *testing.T) {
	ts := startTestServer(t, Options{})
	watcher, changer := ts.dial(t, wire.Hello{}), ts.dial(t, wire.Hello{})
	watched := openKV(t, watcher, wire.KVBucket{Name: "app", Config: true})
	changed := openKV(t, changer, wire.KVBucket{Name: "app", Config: true})

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	st, err := watcher.Open(ctx, wire.KVWatch, wire.KVCall{Handle: watched}, true)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = st.Response(ctx); err != nil {
		t.Fatal(err)
	}
	if first := nextKept(t, st); len(first.Fields) != 0 {
		t.Fatalf("a new config keeps %v", first.Fields)
	}

	change := wire.KVConfigChange{Handle: changed, Set: []string{"port", "4000", "limits.rps", "50"}}
	if _, err = changer.Call(t.Context(), wire.KVConfigure, change); err != nil {
		t.Fatal(err)
	}
	if got := nextKept(t, st); !slices.Equal(got.Fields, []string{"limits.rps", "50", "port", "4000"}) {
		t.Fatalf("after the change the watcher holds %v", got.Fields)
	}
	reset := wire.KVConfigChange{Handle: changed, Reset: []string{"port"}}
	if _, err = changer.Call(t.Context(), wire.KVConfigure, reset); err != nil {
		t.Fatal(err)
	}
	if got := nextKept(t, st); !slices.Equal(got.Fields, []string{"limits.rps", "50"}) {
		t.Fatalf("after the reset the watcher holds %v", got.Fields)
	}

	notJSON := wire.KVConfigChange{Handle: changed, Set: []string{"port", "{"}}
	if _, err = changer.Call(t.Context(), wire.KVConfigure, notJSON); codeOfError(err) != wire.CodeInvalid {
		t.Fatalf("a value that is not JSON: %v", err)
	}
	if err = st.Cancel(); err != nil {
		t.Fatal(err)
	}
	if _, _, err = st.Next(ctx); codeOfError(err) != wire.CodeCancelled {
		t.Fatalf("a cancelled watch ended with %v", err)
	}
}

func nextKept(t *testing.T, st *client.Stream) wire.KVKept {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	body, last, err := st.Next(ctx)
	if err != nil || last {
		t.Fatalf("a watch ended: %v", err)
	}
	var kept wire.KVKept
	if err = kept.Decode(body); err != nil {
		t.Fatal(err)
	}
	return kept
}

// a watch runs until its client leaves: the end of what the client sends, as
// a parent closing a private child's stdin, ends it, so that the connection
// closes rather than wait for a change nobody would read
func TestAWatchEndsWithItsClientsSide(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	handle := openKV(t, conn, wire.KVBucket{Name: "app", Config: true})
	st, err := conn.Open(t.Context(), wire.KVWatch, wire.KVCall{Handle: handle}, true)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = st.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	nextKept(t, st)
	if err = conn.CloseWrite(); err != nil {
		t.Fatal(err)
	}
	select {
	case <-conn.Done():
	case <-time.After(5 * time.Second):
		t.Fatal("the connection stayed open for its watch")
	}
}

// a key's run goes to one client at a time: a second client's kv.run waits
// for the first and is answered what it kept; a run that failed keeps
// nothing, nor does one whose client left, so the next kv.run runs again
func TestAOnceRunsAKeyOnceOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	first, second := ts.dial(t, wire.Hello{}), ts.dial(t, wire.Hello{})
	answers := openKV(t, first, wire.KVBucket{Name: "charges", Once: true})
	others := openKV(t, second, wire.KVBucket{Name: "charges", Once: true})
	key := wire.KVCall{Handle: answers, Key: "req-7"}

	running := openRun(t, first, key, false)
	waiting, err := second.Open(t.Context(), wire.KVRun, wire.KVCall{Handle: others, Key: "req-7"}, false)
	if err != nil {
		t.Fatal(err)
	}
	early, cancel := context.WithTimeout(t.Context(), 100*time.Millisecond)
	defer cancel()
	if _, err = waiting.Response(early); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("a second run of the key while the first runs: %v", err)
	}
	keepAnswer(t, running, wire.KVEntry{Found: true, Value: text("receipt 1")})
	if answer := runResponse(t, waiting); !answer.Found || string(answer.Value.Bytes) != "receipt 1" {
		t.Fatalf("the waiting run was answered %+v", answer)
	}
	if got := mustKV(t, first, wire.KVGet, key); string(got.Value.Bytes) != "receipt 1" {
		t.Fatalf("the kept answer: %+v", got)
	}

	failing := wire.KVCall{Handle: answers, Key: "req-8"}
	keepAnswer(t, openRun(t, first, failing, false), wire.KVEntry{})
	if got := mustKV(t, first, wire.KVGet, failing); got.Found {
		t.Fatalf("a failed run kept %+v", got)
	}
	leaving := openRun(t, first, failing, false)
	if err = leaving.Cancel(); err != nil {
		t.Fatal(err)
	}
	keepAnswer(t, openRun(t, second, wire.KVCall{Handle: others, Key: "req-8"}, false), wire.KVEntry{
		Found: true, Value: text("receipt 2"),
	})

	mustKV(t, first, wire.KVDelete, key)
	if err = openRun(t, first, key, false).Cancel(); err != nil { // a deleted answer runs again
		t.Fatal(err)
	}
	if _, err = kvDo(t, first, wire.KVSet, wire.KVCall{Handle: answers, Key: "x", Value: text("y")}); codeOfError(
		err) != wire.CodeInvalid {
		t.Fatalf("a set of once's answers: %v", err)
	}
}

// openRun opens a kv.run that the client is handed, and checks that it is
func openRun(t *testing.T, conn *client.Conn, ask wire.KVCall, found bool) *client.Stream {
	t.Helper()
	st, err := conn.Open(t.Context(), wire.KVRun, ask, false)
	if err != nil {
		t.Fatal(err)
	}
	if answer := runResponse(t, st); answer.Found != found {
		t.Fatalf("kv.run of %q answered %+v", ask.Key, answer)
	}
	return st
}

func runResponse(t *testing.T, st *client.Stream) wire.KVEntry {
	t.Helper()
	body, err := st.Response(t.Context())
	var answer wire.KVEntry
	if err == nil {
		err = answer.Decode(body)
	}
	if err != nil {
		t.Fatal(err)
	}
	return answer
}

// keepAnswer sends a run's answer and waits for the server's last DATA
func keepAnswer(t *testing.T, st *client.Stream, answer wire.KVEntry) {
	t.Helper()
	if err := st.Send(t.Context(), answer.Append(nil), true); err != nil {
		t.Fatal(err)
	}
	if _, last, err := st.Next(t.Context()); err != nil || !last {
		t.Fatalf("the run's end: last %v, %v", last, err)
	}
}

// a quota over the wire counts a key's use in every window or in none,
// answers its windows in the order its open gave them, and takes uses back
func TestAQuotaOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	ai := openKV(t, conn, wire.KVBucket{Name: "ai", Windows: []wire.KVWindow{
		{Name: "session", Limit: 2, Per: (5 * time.Hour).Milliseconds()},
		{Name: "weekly", Limit: 3, Per: (7 * 24 * time.Hour).Milliseconds()},
	}})
	key := wire.KVCall{Handle: ai, Owners: []string{"tenant-7"}, Key: "user-1"}
	expect := func(got wire.KVAllowance, ok bool, left uint64, used ...uint64) {
		t.Helper()
		if got.OK != ok || got.Left != left || len(got.Windows) != 2 || got.Windows[0].Name != "session" ||
			got.Windows[1].Name != "weekly" || got.Windows[0].Used != used[0] || got.Windows[1].Used != used[1] {
			t.Fatalf("%+v; want OK %v with %d left, used %v", got, ok, left, used)
		}
	}

	expect(allow(t, conn, key), true, 1, 1, 1)
	expect(allow(t, conn, key), true, 0, 2, 2)
	refused := allow(t, conn, key)
	expect(refused, false, 0, 2, 2)
	if refused.RetryAfter < 4*3_600_000 || refused.Windows[0].ResetAt == 0 {
		t.Fatalf("a session out of room: %+v; want to wait about five hours", refused)
	}
	expect(quotaUsage(t, conn, key), false, 0, 2, 2)

	if _, err := conn.Call(t.Context(), wire.KVRefund, key); err != nil {
		t.Fatal(err)
	}
	expect(quotaUsage(t, conn, key), true, 1, 1, 1)
	mustKV(t, conn, wire.KVDelete, key)
	expect(quotaUsage(t, conn, key), true, 2, 0, 0)

	many := key
	many.N = 3
	if _, err := conn.Call(t.Context(), wire.KVAllow, many); codeOfError(err) != wire.CodeInvalid {
		t.Fatalf("three past a session of two: %v", err)
	}
	if _, err := kvDo(t, conn, wire.KVGet, key); codeOfError(err) != wire.CodeInvalid {
		t.Fatalf("a get of a quota: %v", err)
	}
}

func quotaUsage(t *testing.T, conn *client.Conn, ask wire.KVCall) wire.KVAllowance {
	t.Helper()
	body, err := conn.Call(t.Context(), wire.KVUsage, ask)
	var usage wire.KVAllowance
	if err == nil {
		err = usage.Decode(body)
	}
	if err != nil {
		t.Fatal(err)
	}
	return usage
}

// a get or has that names the version it read, or that it read nothing, fails
// conflict once the key is no longer so: the check an SDK's transaction makes
// of every key it read, in the batch that commits its writes
func TestAReadThatNamesWhatItReadFailsOnceTheKeyChanged(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	codes := openKV(t, conn, wire.KVBucket{Name: "codes"})
	sessions := openKV(t, conn, wire.KVBucket{Name: "sessions"})

	read := mustKV(t, conn, wire.KVSet, wire.KVCall{Handle: codes, Key: "K7Q2", Value: text("42")}).Version
	mustKV(t, conn, wire.KVGet, wire.KVCall{Handle: codes, Key: "K7Q2", IfVersion: read})
	mustKV(t, conn, wire.KVHas, wire.KVCall{Handle: codes, Key: "K7Q2", IfVersion: read})
	mustKV(t, conn, wire.KVGet, wire.KVCall{Handle: codes, Key: "none", IfAbsent: true})
	mustKV(t, conn, wire.KVSet, wire.KVCall{Handle: codes, Key: "K7Q2", Value: text("43")})
	for _, stale := range []wire.KVOperation{
		{Method: wire.KVGet, KVCall: wire.KVCall{Handle: codes, Key: "K7Q2", IfVersion: read}},
		{Method: wire.KVHas, KVCall: wire.KVCall{Handle: codes, Key: "K7Q2", IfVersion: read}},
		{Method: wire.KVGet, KVCall: wire.KVCall{Handle: codes, Key: "K7Q2", IfAbsent: true}},
		{Method: wire.KVGet, KVCall: wire.KVCall{Handle: codes, Key: "gone", IfVersion: read}},
	} {
		if _, err := kvDo(t, conn, stale.Method, stale.KVCall); failureOf(err).Code != wire.CodeConflict {
			t.Fatalf("%#04x %+v of a key that changed: %v", uint16(stale.Method), stale.KVCall, err)
		}
	}

	batch := wire.KVCalls{Calls: []wire.KVOperation{
		{Method: wire.KVGet, KVCall: wire.KVCall{Handle: codes, Key: "K7Q2", IfVersion: read}},
		{Method: wire.KVDelete, KVCall: wire.KVCall{Handle: codes, Key: "K7Q2"}},
		{Method: wire.KVSet, KVCall: wire.KVCall{Handle: sessions, Key: "token", Value: text("42")}},
	}}
	_, err := conn.Call(t.Context(), wire.KVBatch, batch)
	if failure := failureOf(err); failure.Code != wire.CodeConflict || failure.What["call"] != "0" {
		t.Fatalf("a batch whose read went stale: %+v", failure)
	}
	if got := mustKV(t, conn, wire.KVGet, wire.KVCall{Handle: sessions, Key: "token"}); got.Found {
		t.Fatal("a batch whose read went stale wrote a session")
	}
	batch.Calls[0].IfVersion = mustKV(t, conn, wire.KVGet, wire.KVCall{Handle: codes, Key: "K7Q2"}).Version
	if _, err = conn.Call(t.Context(), wire.KVBatch, batch); err != nil {
		t.Fatalf("a batch whose read still holds: %v", err)
	}
	if got := mustKV(t, conn, wire.KVGet, wire.KVCall{Handle: sessions, Key: "token"}); !got.Found {
		t.Fatal("a batch whose read still held wrote no session")
	}
}
