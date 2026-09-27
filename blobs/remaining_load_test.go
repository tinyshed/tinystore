package blobs

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"math/rand/v2"
	"slices"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

func noteLatency(measured *latencies, began time.Time) {
	took := time.Since(began)
	measured.mu.Lock()
	measured.took = append(measured.took, took)
	measured.mu.Unlock()
}

func TestStreamingPutMeasured(t *testing.T) {
	measuring(t)
	for _, budget := range []int64{0, inlineSize} {
		t.Run(fmt.Sprint("budget-", budget), func(t *testing.T) {
			s := openTestStoreWith(t, measureDir(t), tinystore.Options{Memory: budget}, Options{})
			media := openTestBucket(t, s, "media")
			const size = 4 << 30
			chunk := randomBytes(64, 64<<10)
			started := time.Now()
			object, err := media.Put(t.Context(), "stream", &endless{chunk: chunk, left: size})
			if err != nil || object.Size != size {
				t.Fatalf("stream: %+v, %v", object, err)
			}
			logRate(t, "stream Put", size, time.Since(started))
			t.Logf("memory %+v", s.runtime.Memory())
			if err = readWhole(t.Context(), media, "stream"); err != nil {
				t.Fatal(err)
			}
			s.closeAndRemove(t)
		})
	}
}

func TestMixedMeasured(t *testing.T) {
	measuring(t)
	s := openTestStoreWith(t, measureDir(t), tinystore.Options{Memory: 1 << 20}, Options{})
	media := openTestBucket(t, s, "media")
	const writers, readers = 8, 8
	data := [][]byte{randomBytes(1, 4<<10), randomBytes(2, 64<<10), randomBytes(3, 1<<20)}
	for n := range writers {
		mustPut(t, media, fmt.Sprint("key/", n), data[0])
	}
	puts, deletes, opens, reads := &latencies{}, &latencies{}, &latencies{}, &latencies{}
	var missing, conflicts atomic.Int64
	var running sync.WaitGroup
	errs := make([]error, writers+readers)
	started := time.Now()
	deadline := started.Add(20 * time.Second)
	for worker := range writers + readers {
		running.Go(func() {
			random := rand.New(rand.NewPCG(uint64(worker), 99))
			for n := 0; time.Now().Before(deadline); n++ {
				began := time.Now()
				var err error
				if worker < writers {
					key := fmt.Sprint("key/", worker)
					if n%5 == 0 {
						err = media.Delete(t.Context(), key)
						noteLatency(deletes, began)
					} else {
						_, err = media.Put(t.Context(), key, bytes.NewReader(data[n%len(data)]))
						noteLatency(puts, began)
					}
				} else {
					var reader *Reader
					var found bool
					reader, found, err = media.Open(t.Context(), fmt.Sprint("key/", random.IntN(writers)))
					noteLatency(opens, began)
					switch {
					case errors.Is(err, tinystore.ErrConflict):
						conflicts.Add(1)
						err = nil
					case err == nil && !found:
						missing.Add(1)
					case err == nil:
						var got []byte
						got, err = io.ReadAll(reader)
						err = errors.Join(err, reader.Close())
						if err == nil && !slices.ContainsFunc(data, func(want []byte) bool { return bytes.Equal(got, want) }) {
							err = fmt.Errorf("a mixed read contains no complete written version")
						}
						noteLatency(reads, began)
					}
				}
				if err != nil {
					errs[worker] = err
					return
				}
			}
		})
	}
	running.Wait()
	for _, err := range errs {
		if err != nil {
			t.Fatal(err)
		}
	}
	for name, measured := range map[string]*latencies{"Put": puts, "Delete": deletes, "Open": opens, "whole read": reads} {
		measured.elapsed = time.Since(started)
		t.Logf("%s: %s", name, measured)
	}
	t.Logf("absent %d, changed-too-often %d, memory %+v", missing.Load(), conflicts.Load(), s.runtime.Memory())
	if usage := s.runtime.Memory(); usage.Used != 0 || usage.Peak > usage.Capacity {
		t.Fatalf("memory after the mixed load: %+v", usage)
	}
	s.maintain(t)
}

func TestConcurrentLargeBoundedMeasured(t *testing.T) {
	measuring(t)
	for _, budget := range []int64{inlineSize, 256 << 10, 4 << 20} {
		t.Run(fmt.Sprint(budget), func(t *testing.T) {
			s := openTestStoreWith(t, measureDir(t), tinystore.Options{Memory: budget}, Options{})
			media := openTestBucket(t, s, "media")
			const callers, size = 64, 64 << 20
			chunk := randomBytes(99, 64<<10)
			heap := watchHeap()
			t.Cleanup(func() { _ = heap() })
			var running sync.WaitGroup
			errs := make([]error, callers)
			started := time.Now()
			for n := range callers {
				running.Go(func() {
					_, errs[n] = media.Put(t.Context(), fmt.Sprint("u/", n), &endless{chunk: chunk, left: size})
				})
			}
			running.Wait()
			took := time.Since(started)
			for _, err := range errs {
				if err != nil {
					t.Fatal(err)
				}
			}
			logRate(t, "64 uploads of 64 MiB", callers*size, took)
			t.Logf("budget %d, memory %+v, sampled heap %s", budget, s.runtime.Memory(), heap())
			if usage := s.runtime.Memory(); usage.Used != 0 || usage.Peak > budget {
				t.Fatalf("memory after concurrent uploads: %+v", usage)
			}
			for _, n := range []int{0, callers - 1} {
				if err := readWhole(t.Context(), media, fmt.Sprint("u/", n)); err != nil {
					t.Fatal(err)
				}
			}
			s.closeAndRemove(t)
		})
	}
}

func TestMillionUsageMeasured(t *testing.T) {
	measuring(t)
	s := openTestStore(t, measureDir(t))
	media := openTestBucket(t, s, "media")
	const count, writers = 1_000_000, 64
	var next atomic.Int64
	var running sync.WaitGroup
	errs := make([]error, writers)
	started := time.Now()
	for worker := range writers {
		running.Go(func() {
			for n := next.Add(1); n <= count; n = next.Add(1) {
				_, err := media.Put(t.Context(), fmt.Sprint("users/42/", n), bytes.NewReader([]byte("x")))
				if err != nil {
					errs[worker] = err
					return
				}
			}
		})
	}
	running.Wait()
	for _, err := range errs {
		if err != nil {
			t.Fatal(err)
		}
	}
	t.Logf("one million populated through Put in %.2f s", time.Since(started).Seconds())
	var took []time.Duration
	for range 5 {
		began := time.Now()
		usage, err := media.Of("users", 42).Usage(t.Context())
		took = append(took, time.Since(began))
		if err != nil || usage.Objects != count || usage.Bytes != count {
			t.Fatalf("usage %+v, %v", usage, err)
		}
	}
	slices.Sort(took)
	t.Logf("Usage million median %.2f ms; all %v", micros(took[2])/1000, took)
	began := time.Now()
	page, err := media.Of("users", 42).Scan(t.Context(), Query{Limit: 1000})
	if err != nil || len(page.Objects) != 1000 || !page.More {
		t.Fatalf("scan: %d, more %v, %v", len(page.Objects), page.More, err)
	}
	t.Logf("Scan first 1000 of million %.2f ms", micros(time.Since(began))/1000)
}

// the real store ticker runs for a full minute; this does not accelerate maintenance
func TestScheduledScrubMeasured(t *testing.T) {
	measuring(t)
	store, err := tinystore.Open(t.Context(), measureDir(t), tinystore.Options{Memory: 2 << 20})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close(context.Background()) })
	started := time.Now()
	engine, err := Open(t.Context(), store, Options{})
	if err != nil {
		t.Fatal(err)
	}
	media, err := OpenBucket(t.Context(), engine, "media")
	if err != nil {
		t.Fatal(err)
	}
	data := randomBytes(64, 64<<10)
	for n := range 256 {
		mustPut(t, media, fmt.Sprint(n), data)
	}
	var whole, opening [3]latencies
	var running sync.WaitGroup
	errs := make([]error, 16)
	t.Log("reading across the actual one-minute maintenance tick, until 75 seconds after Open")
	for worker := range 16 {
		running.Go(func() {
			random := rand.New(rand.NewPCG(uint64(worker), 23))
			for time.Since(started) < 75*time.Second {
				began := time.Now()
				window := 0
				if since := began.Sub(started); since >= 65*time.Second {
					window = 2
				} else if since >= 55*time.Second {
					window = 1
				}
				reader, found, openErr := media.Open(t.Context(), fmt.Sprint(random.IntN(256)))
				noteLatency(&opening[window], began)
				if openErr != nil || !found {
					errs[worker] = fmt.Errorf("scheduled Open: found %v, %w", found, openErr)
					return
				}
				_, readErr := io.Copy(io.Discard, reader)
				if readErr = errors.Join(readErr, reader.Close()); readErr != nil {
					errs[worker] = readErr
					return
				}
				noteLatency(&whole[window], began)
			}
		})
	}
	running.Wait()
	for _, err = range errs {
		if err != nil {
			t.Fatal(err)
		}
	}
	for window, length := range []time.Duration{55 * time.Second, 10 * time.Second, 10 * time.Second} {
		opening[window].elapsed, whole[window].elapsed = length, length
		t.Logf("window %d Open %s; whole %s", window, &opening[window], &whole[window])
	}
	var place scrubPlace
	err = engine.file.Lookup(t.Context(), func(r sqlite.Reader) error {
		return sqlite.QueryRow(t.Context(), r, selectScrub).Scan(&place.content, &place.offset, &place.state, &place.pace)
	})
	if err != nil || place.content == 0 || place.pace != scrubBuffer {
		t.Fatalf("scheduled scrub did not advance: %+v, %v", place, err)
	}
	t.Logf("scheduled scrub content %d offset %d pace %d; memory %+v", place.content, place.offset, place.pace, store.Memory())
}
