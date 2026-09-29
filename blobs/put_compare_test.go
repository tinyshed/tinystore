package blobs

import (
	"bytes"
	"fmt"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

// Fixed counts, so that every run compared sees the same bytes and keys, reads
// after the writes included.
func TestPutCompared(t *testing.T) {
	measuring(t)
	for _, fixture := range []struct{ size, callers, count int }{
		{4 << 10, 128, 16384},
		{64 << 10, 1, 128},
		{64 << 10, 128, 2048},
		{1 << 20, 64, 1024},
	} {
		t.Run(fmt.Sprintf("%dKiB/%d", fixture.size>>10, fixture.callers), func(t *testing.T) {
			s := openTestStore(t, measureDir(t))
			media := openTestBucket(t, s, "media")
			data := randomBytes(uint64(fixture.size), fixture.size)
			var next atomic.Int64
			measured := &latencies{}
			errs := make([]error, fixture.callers)
			var running sync.WaitGroup
			started := time.Now()
			for caller := range fixture.callers {
				running.Go(func() {
					for n := next.Add(1); n <= int64(fixture.count); n = next.Add(1) {
						began := time.Now()
						_, err := media.Put(t.Context(), fmt.Sprint("item/", n), bytes.NewReader(data))
						if err != nil {
							errs[caller] = err
							return
						}
						took := time.Since(began)
						measured.mu.Lock()
						measured.took = append(measured.took, took)
						measured.mu.Unlock()
					}
				})
			}
			running.Wait()
			measured.elapsed = time.Since(started)
			for _, err := range errs {
				if err != nil {
					t.Fatal(err)
				}
			}
			t.Logf("%d objects of %d bytes: %s", fixture.count, fixture.size, measured)
			for _, n := range []int{1, fixture.count / 2, fixture.count} {
				got, _, found := readAll(t, media, fmt.Sprint("item/", n))
				if !found || !bytes.Equal(got, data) {
					t.Fatalf("object %d differs after the timed writes", n)
				}
			}
			s.closeAndRemove(t)
		})
	}
}
