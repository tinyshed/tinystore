package kv

import (
	"bytes"
	"database/sql"
	"errors"
	"fmt"
	"math/rand/v2"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// an expired key is absent to every operation, whether maintenance has
// deleted its row or not
func TestAnExpiredKeyIsAbsentToEveryOperation(t *testing.T) {
	state := openTestState(t, t.TempDir())
	bucket := openTestBucket[string](t, state, "short")
	old, err := bucket.SetEntry(t.Context(), "k", "old", TTL(time.Minute))
	if err != nil {
		t.Fatal(err)
	}
	state.clock.advance(2 * time.Minute)

	if _, found, getErr := bucket.Get(t.Context(), "k"); found || getErr != nil {
		t.Fatalf("Get found an expired key: %v", getErr)
	}
	if found, hasErr := bucket.Has(t.Context(), "k"); found || hasErr != nil {
		t.Fatalf("Has found an expired key: %v", hasErr)
	}
	if page, scanErr := bucket.Scan(t.Context(), Query{}); len(page.Entries) != 0 || scanErr != nil {
		t.Fatalf("Scan found an expired key: %v", scanErr)
	}
	if _, found, takeErr := bucket.Take(t.Context(), "k"); found || takeErr != nil {
		t.Fatalf("Take found an expired key: %v", takeErr)
	}
	if found, touchErr := bucket.Touch(t.Context(), "k", TTL(time.Hour)); found || touchErr != nil {
		t.Fatalf("Touch renewed an expired key: %v", touchErr)
	}
	if err = bucket.Set(t.Context(), "k", "new", IfVersion(old.Version)); !errors.Is(err, tinystore.ErrConflict) {
		t.Fatalf("IfVersion passed on an expired key: %v", err)
	}
	if err = bucket.Delete(t.Context(), "k", IfVersion(old.Version)); !errors.Is(err, tinystore.ErrConflict) {
		t.Fatalf("a Delete with IfVersion passed on an expired key: %v", err)
	}
	created, err := bucket.SetIfAbsent(t.Context(), "k", "claimed")
	if err != nil || !created {
		t.Fatalf("SetIfAbsent did not claim an expired key: %v", err)
	}
	if value, _, _ := bucket.Get(t.Context(), "k"); value != "claimed" {
		t.Fatalf("the claimed key holds %q", value)
	}
}

// a key gets the bucket's DefaultTTL once, when it is created: a later Set
// without a TTL keeps its expiry, so a window does not slide
func TestADefaultTTLIsGivenOnceAtCreation(t *testing.T) {
	state := openTestState(t, t.TempDir())
	bucket := openTestBucket[int](t, state, "window", DefaultTTL(15*time.Minute))
	first, err := bucket.SetEntry(t.Context(), "k", 1)
	if err != nil {
		t.Fatal(err)
	}
	state.clock.advance(10 * time.Minute)
	second, err := bucket.SetEntry(t.Context(), "k", 2)
	if err != nil || !second.ExpiresAt.Equal(first.ExpiresAt) {
		t.Fatalf("a second Set moved the expiry from %v to %v: %v", first.ExpiresAt, second.ExpiresAt, err)
	}
	state.clock.advance(6 * time.Minute)
	if _, found, _ := bucket.Get(t.Context(), "k"); found {
		t.Fatal("the key outlived its window")
	}
	renewed, err := bucket.SetEntry(t.Context(), "k", 3, TTL(time.Hour))
	if err != nil || !renewed.ExpiresAt.Equal(state.clock.Now().Add(time.Hour)) {
		t.Fatalf("an explicit TTL gave %v: %v", renewed.ExpiresAt, err)
	}
}

// a version never repeats: not after a delete, an expiry or a reopen, so an
// old IfVersion cannot pass against a key written again
func TestAVersionNeverRepeatsAfterDeleteExpiryOrReopen(t *testing.T) {
	state := openTestState(t, t.TempDir())
	bucket := openTestBucket[string](t, state, "versions")
	seen := map[Version]bool{}
	write := func(bucket *Bucket[string], options ...Option) Version {
		t.Helper()
		entry, err := bucket.SetEntry(t.Context(), "k", "v", options...)
		if err != nil || seen[entry.Version] {
			t.Fatalf("version %v again, %v", entry.Version, err)
		}
		seen[entry.Version] = true
		return entry.Version
	}

	first := write(bucket)
	if err := bucket.Delete(t.Context(), "k"); err != nil {
		t.Fatal(err)
	}
	write(bucket, TTL(time.Minute))
	state.clock.advance(2 * time.Minute)
	write(bucket)

	state = state.reopen(t)
	bucket = openTestBucket[string](t, state, "versions")
	write(bucket)
	if err := bucket.Set(t.Context(), "k", "stale", IfVersion(first)); !errors.Is(err, tinystore.ErrConflict) {
		t.Fatalf("an old version passed: %v", err)
	}
}

// of the callers taking one key, one gets its value
func TestConcurrentTakesGiveTheValueOnce(t *testing.T) {
	state := openTestState(t, t.TempDir())
	codes := openTestBucket[int64](t, state, "codes")
	if err := codes.Set(t.Context(), "digest", 42); err != nil {
		t.Fatal(err)
	}
	var wg sync.WaitGroup
	var mu sync.Mutex
	winners := 0
	for range 64 {
		wg.Go(func() {
			userID, found, err := codes.Take(t.Context(), "digest")
			if err != nil || (found && userID != 42) {
				t.Errorf("Take: %d, %v", userID, err)
			}
			if found {
				mu.Lock()
				winners++
				mu.Unlock()
			}
		})
	}
	wg.Wait()
	if winners != 1 {
		t.Fatalf("%d callers took the one value", winners)
	}
}

// refusingCodec writes a string's bytes and refuses to read them back, or
// panics reading them
type refusingCodec struct{ panics bool }

func (refusingCodec) Encode(s string) ([]byte, error) { return []byte(s), nil }

func (c refusingCodec) Decode([]byte) (string, error) {
	if c.panics {
		panic("a codec that panics")
	}
	return "", errors.New("a codec that refuses")
}

// A Take whose value no longer decodes is ErrCorrupt and keeps the value, a
// spilled one too, in a group and inside Tx. A codec's panic is such a failure
// of its own write rather than of the writes beside it.
func TestAFailedTakeKeepsItsValue(t *testing.T) {
	state := openTestState(t, t.TempDir())
	for _, panics := range []bool{false, true} {
		values := openTestBucket[string](t, state, fmt.Sprintf("refused-%v", panics),
			WithCodec[string](refusingCodec{panics: panics}))
		for key, value := range map[string]string{"inline": "payload", "spilled": strings.Repeat("s", inlineLimit+1)} {
			if err := values.Set(t.Context(), key, value); err != nil {
				t.Fatal(err)
			}
			if _, _, err := values.Take(t.Context(), key); !errors.Is(err, tinystore.ErrCorrupt) {
				t.Fatalf("a Take of a value that no longer decodes: %v", err)
			}
			err := state.Tx(t.Context(), func(tx *Tx) error {
				if _, _, takeErr := values.WithTx(tx).Take(t.Context(), key); !errors.Is(takeErr, tinystore.ErrCorrupt) {
					t.Errorf("the same Take inside Tx: %v", takeErr)
				}
				return nil
			})
			if err != nil {
				t.Fatal(err)
			}
			if found, hasErr := values.Has(t.Context(), key); !found || hasErr != nil {
				t.Fatalf("the refused Takes of %s took it: %v, %v", key, found, hasErr)
			}
		}
	}
	if rows := state.spilledRows(t); rows != 2 {
		t.Fatalf("%d spilled rows after refused Takes of two spilled values", rows)
	}
}

// a handler whose claim expired can neither finish nor delete the claim that
// came after it
func TestAStaleClaimCannotFinishOrDeleteTheNext(t *testing.T) {
	state := openTestState(t, t.TempDir())
	seen := openTestBucket[struct{}](t, state, "events")
	first, created, err := seen.SetEntryIfAbsent(t.Context(), "evt_1", struct{}{}, TTL(10*time.Minute))
	if err != nil || !created {
		t.Fatalf("the first claim: %v, %v", created, err)
	}
	if _, again, _ := seen.SetEntryIfAbsent(t.Context(), "evt_1", struct{}{}); again {
		t.Fatal("a second claim while the first is live")
	}
	state.clock.advance(11 * time.Minute)
	second, created, err := seen.SetEntryIfAbsent(t.Context(), "evt_1", struct{}{}, TTL(10*time.Minute))
	if err != nil || !created {
		t.Fatalf("the claim after it expired: %v, %v", created, err)
	}

	if err = seen.Delete(t.Context(), "evt_1", IfVersion(first.Version)); !errors.Is(err, tinystore.ErrConflict) {
		t.Fatalf("the stale claim deleted the next: %v", err)
	}
	err = seen.Set(t.Context(), "evt_1", struct{}{}, IfVersion(first.Version), TTL(7*24*time.Hour))
	if !errors.Is(err, tinystore.ErrConflict) {
		t.Fatalf("the stale claim finished over the next: %v", err)
	}
	if err = seen.Set(t.Context(), "evt_1", struct{}{}, IfVersion(second.Version), TTL(7*24*time.Hour)); err != nil {
		t.Fatalf("the live claim could not finish: %v", err)
	}
}

// of two writes naming the version they read, one passes
func TestOneOfTwoVersionedWritesConflicts(t *testing.T) {
	state := openTestState(t, t.TempDir())
	drafts := openTestBucket[string](t, state, "drafts")
	read, err := drafts.Of(7).SetEntry(t.Context(), 1, "draft")
	if err != nil {
		t.Fatal(err)
	}
	errs := make([]error, 2)
	var wg sync.WaitGroup
	for tab := range errs {
		wg.Go(func() { _, errs[tab] = drafts.Of(7).SetEntry(t.Context(), 1, "tab", IfVersion(read.Version)) })
	}
	wg.Wait()
	conflicts := 0
	for _, err := range errs {
		switch {
		case errors.Is(err, tinystore.ErrConflict):
			conflicts++
		case err != nil:
			t.Fatal(err)
		}
	}
	if conflicts != 1 {
		t.Fatalf("%d of two versioned writes conflicted", conflicts)
	}
}

// a value over 512 bytes lives in spilled, and leaves it with its key
func TestALargeValueSpillsAndReadsBack(t *testing.T) {
	state := openTestState(t, t.TempDir())
	bucket := openTestBucket[[]byte](t, state, "large")
	large := bytes.Repeat([]byte("v"), inlineLimit+1)
	largest := bytes.Repeat([]byte("w"), maxValue)
	for key, value := range map[string][]byte{"a": large, "b": largest} {
		if got := roundTripKey(t, bucket, key, value); !bytes.Equal(got, value) {
			t.Fatalf("a spilled value of %d bytes came back as %d", len(value), len(got))
		}
	}
	if spilled := state.spilledRows(t); spilled != 2 {
		t.Fatalf("%d spilled rows, want 2", spilled)
	}
	if err := bucket.Set(t.Context(), "a", []byte("small")); err != nil {
		t.Fatal(err)
	}
	if _, found, err := bucket.Take(t.Context(), "b"); !found || err != nil {
		t.Fatalf("Take of a spilled value: %v, %v", found, err)
	}
	if spilled := state.spilledRows(t); spilled != 0 {
		t.Fatalf("%d spilled rows after their keys moved on", spilled)
	}
	if err := bucket.Set(t.Context(), "c", append(largest, 'x')); !errors.Is(err, tinystore.ErrLimit) {
		t.Fatalf("a value over 1 MiB: %v", err)
	}
}

func roundTripKey[V any](t *testing.T, bucket *Bucket[V], key string, value V) V {
	t.Helper()
	if err := bucket.Set(t.Context(), key, value); err != nil {
		t.Fatal(err)
	}
	got, found, err := bucket.Get(t.Context(), key)
	if err != nil || !found {
		t.Fatalf("%v, %v", found, err)
	}
	return got
}

func (s *testState) spilledRows(t *testing.T) int {
	t.Helper()
	var count int
	err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select count(*) from spilled`).Scan(&count)
	})
	if err != nil {
		t.Fatal(err)
	}
	return count
}

// the writer keeps 4 MiB of pages, the readers 1 MiB each
func TestTheWriterKeepsFourMiBOfPages(t *testing.T) {
	state := openTestState(t, t.TempDir())
	var kib int64
	err := state.file.UpdatePrepared(t.Context(), func(w sqlite.Writer) error {
		return sqlite.QueryRow(t.Context(), w, "pragma cache_size").Scan(&kib)
	})
	if err != nil || kib != -4096 {
		t.Fatalf("the writer's cache_size is %d, %v", kib, err)
	}
	err = state.file.Lookup(t.Context(), func(r sqlite.Reader) error {
		return sqlite.QueryRow(t.Context(), r, "pragma cache_size").Scan(&kib)
	})
	if err != nil || kib != -1024 {
		t.Fatalf("a reader's cache_size is %d, %v", kib, err)
	}
}

// the goroutines a processor writes from: 512 at sixteen
const writersPerCPU = 32

// BenchmarkRandomSetsByWriterCache overwrites keys at random, 64 bytes each,
// from many goroutines at once, with the writer's page cache at 1, 4, 8 and 16
// MiB, in a file of 20,000 keys and of 200,000. It shows what a larger cache is
// worth to grouped writes that land on pages all over the file.
func BenchmarkRandomSetsByWriterCache(b *testing.B) {
	for _, count := range []int{20_000, 200_000} {
		for _, mib := range []int{1, 4, 8, 16} {
			b.Run(fmt.Sprintf("keys=%d/cache=%dMiB", count, mib), func(b *testing.B) {
				overwriteWithWriterCache(b, count, mib)
			})
		}
	}
}

func overwriteWithWriterCache(b *testing.B, count, mib int) {
	state := openBenchmarkStore(b)
	sessions, err := OpenBucket[string](b.Context(), state, "sessions", DefaultTTL(time.Hour))
	if err != nil {
		b.Fatal(err)
	}
	keys, value := overwrittenKeysOf(b, state, sessions, count), strings.Repeat("v", 64)
	err = state.file.UpdatePrepared(b.Context(), func(w sqlite.Writer) error {
		_, pragmaErr := w.ExecContext(b.Context(), fmt.Sprintf("pragma cache_size(-%d)", mib<<10))
		return pragmaErr
	})
	if err != nil {
		b.Fatal(err)
	}
	before, err := state.file.WriterCounters(b.Context())
	if err != nil {
		b.Fatal(err)
	}

	var writers atomic.Uint64
	b.SetParallelism(writersPerCPU)
	b.ResetTimer()
	b.RunParallel(func(pb *testing.PB) {
		random := rand.New(rand.NewPCG(13, writers.Add(1)))
		for pb.Next() {
			if setErr := sessions.Set(b.Context(), keys[random.IntN(len(keys))], value); setErr != nil {
				b.Error(setErr)
				return
			}
		}
	})
	b.StopTimer()
	reportWriterCache(b, state, before)
}

// overwrittenKeysOf writes the keys an overwrite chooses among, a thousand a
// transaction, and returns them
func overwrittenKeysOf(b *testing.B, state *Store, sessions *Bucket[string], count int) []string {
	b.Helper()
	keys := make([]string, count)
	for i := range keys {
		keys[i] = fmt.Sprintf("session-%08d", i)
	}
	for from := 0; from < len(keys); from += 1000 {
		err := state.Tx(b.Context(), func(tx *Tx) error {
			for _, key := range keys[from:min(from+1000, len(keys))] {
				if err := sessions.WithTx(tx).Set(b.Context(), key, "first"); err != nil {
					return err
				}
			}
			return nil
		})
		if err != nil {
			b.Fatal(err)
		}
	}
	return keys
}

// reportWriterCache reports the Sets a second and what the writer's cache
// missed and spilled for each of them
func reportWriterCache(b *testing.B, state *Store, before sqlite.WriterCounters) {
	b.Helper()
	after, err := state.file.WriterCounters(b.Context())
	if err != nil {
		b.Fatal(err)
	}
	b.ReportMetric(float64(b.N)/b.Elapsed().Seconds(), "sets/s")
	b.ReportMetric(float64(after.CacheMisses-before.CacheMisses)/float64(b.N), "misses/set")
	b.ReportMetric(float64(after.CacheSpills-before.CacheSpills)/float64(b.N), "spills/set")
	b.ReportMetric(float64(after.Commits-before.Commits)/float64(b.N), "commits/set")
}

// a Touch renews a live key and keeps its version, so a renewal fails no
// IfVersion; a key a Touch does not find stays absent
func TestTouchRenewsAndKeepsTheVersion(t *testing.T) {
	state := openTestState(t, t.TempDir())
	sessions := openTestBucket[string](t, state, "sessions")
	written, err := sessions.SetEntry(t.Context(), "token", "s", TTL(time.Minute))
	if err != nil {
		t.Fatal(err)
	}
	state.clock.advance(50 * time.Second)
	if found, touchErr := sessions.Touch(t.Context(), "token", TTL(time.Minute)); !found || touchErr != nil {
		t.Fatalf("Touch: %v, %v", found, touchErr)
	}
	state.clock.advance(50 * time.Second)
	entry, found, err := sessions.GetEntry(t.Context(), "token")
	if err != nil || !found || entry.Version != written.Version {
		t.Fatalf("after Touch: %v, %v, version %v against %v", found, err, entry.Version, written.Version)
	}
	if _, err = sessions.Touch(t.Context(), "token"); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a Touch with no expiry to give: %v", err)
	}
}

func TestABucketCannotChangeItsKindUnderItsData(t *testing.T) {
	state := openTestState(t, t.TempDir())
	openTestCounters(t, state, "hits")
	if _, err := OpenBucket[int](t.Context(), state.Store, "hits"); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a bucket of counters opened for values: %v", err)
	}
	openTestBucket[int](t, state, "scores")
	if _, err := OpenCounters(t.Context(), state.Store, "scores"); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a bucket of values opened as counters: %v", err)
	}
	if _, err := OpenBucket[int](t.Context(), state.Store, "Hits!"); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a name that is not plain: %v", err)
	}
}
