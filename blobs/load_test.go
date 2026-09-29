package blobs

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"math/rand/v2"
	"os"
	"runtime"
	"slices"
	"strconv"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

// Measurements of the engine, skipped unless TINYSTORE_SPIKE=1. Each has the
// shape of a measurement of the prototype in spike/blobs_*, so that both run in
// one session with matching payloads.
//
// TINYSTORE_BLOBS_DIR puts their files on a chosen disk, and
// TINYSTORE_BLOBS_SECONDS sets a load's length.

func measuring(t *testing.T) {
	t.Helper()
	if os.Getenv("TINYSTORE_SPIKE") != "1" {
		t.Skip("a measurement: set TINYSTORE_SPIKE=1")
	}
}

// measureDir is a directory for one measurement, removed when the test ends. It
// is under TINYSTORE_BLOBS_DIR when that is set, so that a container writes to
// a volume rather than to the bind mount.
func measureDir(t *testing.T) string {
	t.Helper()
	root := os.Getenv("TINYSTORE_BLOBS_DIR")
	if root == "" {
		return t.TempDir()
	}
	dir, err := os.MkdirTemp(root, "engine-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return dir
}

func loadLength() time.Duration {
	if seconds, err := strconv.Atoi(os.Getenv("TINYSTORE_BLOBS_SECONDS")); err == nil && seconds > 0 {
		return time.Duration(seconds) * time.Second
	}
	return 3 * time.Second
}

// latencies is one load's calls: how long each took, and the load's length
type latencies struct {
	mu      sync.Mutex
	took    []time.Duration
	elapsed time.Duration
}

func (l *latencies) String() string {
	took := slices.Clone(l.took)
	slices.Sort(took)
	if len(took) == 0 {
		return "no calls"
	}
	return fmt.Sprintf("%8.0f a second  p50 %7.0f µs p99 %7.0f µs", float64(len(took))/l.elapsed.Seconds(),
		micros(took[len(took)/2]), micros(took[len(took)*99/100]))
}

func micros(d time.Duration) float64 { return float64(d) / float64(time.Microsecond) }

// load calls call from callers goroutines, each with its own counter, until
// length has passed, and times each call
func load(t *testing.T, callers int, length time.Duration, call func(caller, n int) error) *latencies {
	t.Helper()
	measured := &latencies{}
	started := time.Now()
	deadline := started.Add(length)
	errs := make([]error, callers)
	var running sync.WaitGroup
	for caller := range callers {
		running.Go(func() {
			for n := 0; time.Now().Before(deadline) && errs[caller] == nil; n++ {
				began := time.Now()
				errs[caller] = call(caller, n)
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
	return measured
}

// readWhole opens key and reads it through to its end, which checks a file
func readWhole(ctx context.Context, bucket *Bucket, key string) error {
	reader, found, err := bucket.Open(ctx, key)
	if err != nil || !found {
		return fmt.Errorf("open %s: %v, %w", key, found, err)
	}
	_, err = io.Copy(io.Discard, reader)
	return errorsJoin(err, reader.Close())
}

func errorsJoin(first, second error) error {
	if first != nil {
		return first
	}
	return second
}

// TestPlacesMeasured writes and reads objects of 4 to 256 KiB from 1, 16 and
// 128 callers through the engine, as spike's TestBlobsPlaces writes and reads
// them through the prototype. Each Put has a new key, and the Opens read whole
// at random among the caller's keys.
func TestPlacesMeasured(t *testing.T) {
	measuring(t)
	for _, size := range []int{4 << 10, 16 << 10, 64 << 10, 256 << 10} {
		s := openTestStore(t, measureDir(t))
		media := openTestBucket(t, s, "media")
		data := randomBytes(uint64(size), size)
		for _, callers := range []int{1, 16, 128} {
			written := make([][]string, callers)
			label := fmt.Sprintf("engine %7s %3d callers", sizeText(size), callers)
			puts := load(t, callers, loadLength(), func(caller, n int) error {
				key := fmt.Sprintf("c%d/%d/o%d", callers, caller, n)
				written[caller] = append(written[caller], key)
				_, err := media.Put(t.Context(), key, bytes.NewReader(data))
				return err
			})
			t.Logf("%s  Put  %s", label, puts)
			opens := load(t, callers, loadLength(), func(caller, _ int) error {
				mine := written[caller]
				return readWhole(t.Context(), media, mine[rand.IntN(len(mine))])
			})
			t.Logf("%s  Open %s", label, opens)
		}
		t.Logf("engine %7s  renames a scanner made wait: %d", sizeText(size), s.retries.Load())
		s.closeAndRemove(t)
	}
}

func sizeText(size int) string {
	switch {
	case size >= 1<<30 && size%(1<<30) == 0:
		return fmt.Sprintf("%d GiB", size>>30)
	case size >= 1<<20 && size%(1<<20) == 0:
		return fmt.Sprintf("%d MiB", size>>20)
	}
	return fmt.Sprintf("%d KiB", size>>10)
}

// TestLargeUploadMeasured puts 4 GiB through Put's bounded transfer buffer and
// through an upload written 64 KiB at a time, as spike's TestBlobsLargeUpload
// writes its file. It then reads the object whole, checked, and in ranges of 4
// MiB at random offsets, unchecked.
func TestLargeUploadMeasured(t *testing.T) {
	measuring(t)
	const size = 4 << 30
	s := openTestStore(t, measureDir(t))
	media := openTestBucket(t, s, "media")
	chunk := randomBytes(64, 64<<10)

	began := time.Now()
	if _, err := media.Put(t.Context(), "put", &endless{chunk: chunk, left: size}); err != nil {
		t.Fatal(err)
	}
	logRate(t, "Put, bounded transfer buffer", size, time.Since(began))
	began = time.Now()
	upload, err := media.Create(t.Context(), "written", Size(size))
	if err != nil {
		t.Fatal(err)
	}
	for written := 0; written < size; written += len(chunk) {
		if _, err = upload.Write(chunk); err != nil {
			t.Fatal(err)
		}
	}
	wrote := time.Since(began)
	if _, err = upload.Commit(t.Context()); err != nil {
		t.Fatal(err)
	}
	logRate(t, "Create, Write 64 KiB a time", size, time.Since(began))
	t.Logf("%-32s %9.1f ms", "  of which the Commit", micros(time.Since(began)-wrote)/1000)

	began = time.Now()
	if err = readWhole(t.Context(), media, "put"); err != nil {
		t.Fatal(err)
	}
	logRate(t, "a whole read, checked", size, time.Since(began))
	logRanges(t, media)
}

func logRate(t *testing.T, what string, size int64, took time.Duration) {
	t.Helper()
	t.Logf("%-32s %6s in %7.2f s, %5.2f GB/s", what, sizeText(int(size)), took.Seconds(),
		float64(size)/took.Seconds()/1e9)
}

// logRanges reads a thousand ranges of 4 MiB at random offsets of "put"
func logRanges(t *testing.T, media *Bucket) {
	t.Helper()
	reader, _, err := media.Open(t.Context(), "put")
	if err != nil {
		t.Fatal(err)
	}
	defer reader.Close()
	const ranges, length = 1000, 4 << 20
	buffer := make([]byte, length)
	random := rand.New(rand.NewPCG(1, 2))
	began := time.Now()
	for range ranges {
		if _, err = reader.ReadAt(buffer, random.Int64N(reader.Size-length)); err != nil {
			t.Fatal(err)
		}
	}
	took := time.Since(began)
	t.Logf("%-32s %6.0f of 4 MiB a second, %5.2f GB/s", "ranges, unchecked", ranges/took.Seconds(),
		float64(ranges*length)/took.Seconds()/1e9)
}

// TestUploadsAtOnceMeasured runs 16 to 1024 Puts of 1 MiB at once, 4 GiB in
// all, as spike's TestBlobsUploadsAtOnce runs them through the prototype. It
// reports their rate and the sampled Go heap in use.
func TestUploadsAtOnceMeasured(t *testing.T) {
	measuring(t)
	chunk := randomBytes(99, 64<<10)
	for _, uploads := range []int{16, 64, 256, 1024} {
		s := openTestStore(t, measureDir(t))
		media := openTestBucket(t, s, "media")
		heap := watchHeap()
		t.Cleanup(func() { _ = heap() })
		count := (4 << 30) / (1 << 20)
		var next atomic.Int64
		began := time.Now()
		var running sync.WaitGroup
		errs := make([]error, uploads)
		for upload := range uploads {
			running.Go(func() {
				for n := next.Add(1); n <= int64(count) && errs[upload] == nil; n = next.Add(1) {
					_, errs[upload] = media.Put(t.Context(), fmt.Sprint("u/", n), &endless{chunk: chunk, left: 1 << 20})
				}
			})
		}
		running.Wait()
		took := time.Since(began)
		for _, err := range errs {
			if err != nil {
				t.Fatal(err)
			}
		}
		t.Logf("%4d Puts of 1 MiB at once  %6.0f a second, %5.2f GB/s; heap at most %s", uploads,
			float64(count)/took.Seconds(), float64(count<<20)/took.Seconds()/1e9, heap())
		s.closeAndRemove(t)
	}
}

// closeAndRemove closes the store and removes its files, so that the next
// round does not retain the preceding round's files
func (s *testStore) closeAndRemove(t *testing.T) {
	t.Helper()
	if err := s.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := os.RemoveAll(s.dir); err != nil {
		t.Fatal(err)
	}
}

// watchHeap samples the Go heap in use every 20 ms until the returned
// function answers the most it saw
func watchHeap() func() string {
	runtime.GC()
	var peak atomic.Uint64
	stop, done := make(chan struct{}), make(chan struct{})
	var once sync.Once
	go func() {
		defer close(done)
		var stats runtime.MemStats
		for {
			runtime.ReadMemStats(&stats)
			if stats.HeapInuse > peak.Load() {
				peak.Store(stats.HeapInuse)
			}
			select {
			case <-stop:
				return
			case <-time.After(20 * time.Millisecond):
			}
		}
	}()
	return func() string {
		once.Do(func() { close(stop) })
		<-done
		return fmt.Sprintf("%.0f MiB", float64(peak.Load())/(1<<20))
	}
}

// TestScrubBesideReadersMeasured times Opens of 64 KiB files read whole from 16
// callers alone, and then beside the scrub reading as fast as it can, far past
// its normal one-minute spacing. It measures readers under that stress.
func TestScrubBesideReadersMeasured(t *testing.T) {
	measuring(t)
	s := openTestStore(t, measureDir(t))
	media := openTestBucket(t, s, "media")
	const files, callers = 4096, 16
	data := randomBytes(64, 64<<10)
	for n := range files {
		mustPut(t, media, fmt.Sprint("f/", n), data)
	}
	opens := func() *latencies {
		return load(t, callers, loadLength(), func(int, int) error {
			return readWhole(t.Context(), media, fmt.Sprint("f/", rand.IntN(files)))
		})
	}
	t.Logf("Open of 64 KiB, %d callers, alone         %s", callers, opens())

	var scrubbed atomic.Int64
	stop, done := make(chan struct{}), make(chan struct{})
	var stopOnce sync.Once
	stopScrub := func() {
		stopOnce.Do(func() { close(stop) })
		<-done
	}
	go func() {
		defer close(done)
		for {
			select {
			case <-stop:
				return
			default:
			}
			read, _, err := s.scrub(t.Context())
			if err != nil {
				t.Error(err)
				return
			}
			scrubbed.Add(read)
		}
	}()
	t.Cleanup(stopScrub)
	began := time.Now()
	beside := opens()
	stopScrub()
	t.Logf("Open of 64 KiB, %d callers, beside a scrub %s", callers, beside)
	t.Logf("the scrub read %.2f GB/s meanwhile", float64(scrubbed.Load())/time.Since(began).Seconds()/1e9)
}

// TestUsageMeasured counts a folder of 10⁵ objects, as spike's TestBlobsUsage
// counts 10⁵ rows of the prototype's objects
func TestUsageMeasured(t *testing.T) {
	measuring(t)
	s := openTestStore(t, measureDir(t))
	media := openTestBucket(t, s, "media")
	const objects, writers = 100_000, 64
	var next atomic.Int64
	var running sync.WaitGroup
	for range writers {
		running.Go(func() {
			for n := next.Add(1); n <= objects; n = next.Add(1) {
				if _, err := media.Put(t.Context(), fmt.Sprint("users/42/", n), bytes.NewReader([]byte("x"))); err != nil {
					t.Error(err)
					return
				}
			}
		})
	}
	running.Wait()
	var took []time.Duration
	for range 5 {
		began := time.Now()
		usage, err := media.Of("users", 42).Usage(t.Context())
		took = append(took, time.Since(began))
		if err != nil || usage.Objects != objects {
			t.Fatalf("usage %+v, %v", usage, err)
		}
	}
	slices.Sort(took)
	t.Logf("Usage over %d objects: %.1f ms, the median of five", objects, micros(took[2])/1000)
}
