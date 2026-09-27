package spike

import (
	"crypto/sha256"
	"fmt"
	"hash"
	"io"
	"math/rand/v2"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"sync"
	"testing"
	"time"
)

// The blobs round of docs/blobs.md measures the mechanics before a blobs
// package exists: where an object's bytes are kept and what each place costs
// to write, read, sync, snapshot and back up; how many files a directory tree
// takes; what a reader may keep through a delete on each system; and what a
// check of the bytes costs. The measurements need TINYSTORE_SPIKE=1;
// TINYSTORE_BLOBS_DIR puts their files on a chosen disk, TINYSTORE_BLOBS_SECONDS
// sets each load's length and TINYSTORE_BLOBS_FILES the files of the count.

// blobsDir is a directory for one measurement: under TINYSTORE_BLOBS_DIR when
// it is set, so that a container writes to a volume rather than to the bind
// mount, and removed when the test ends
func blobsDir(t *testing.T) string {
	t.Helper()
	root := os.Getenv("TINYSTORE_BLOBS_DIR")
	if root == "" {
		return t.TempDir()
	}
	dir, err := os.MkdirTemp(root, "blobs-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return dir
}

func blobsSeconds() time.Duration {
	if seconds, err := strconv.Atoi(os.Getenv("TINYSTORE_BLOBS_SECONDS")); err == nil && seconds > 0 {
		return time.Duration(seconds) * time.Second
	}
	return 3 * time.Second
}

// blobsFileCount is the files of the count, a million unless it says
func blobsFileCount() int {
	if count, err := strconv.Atoi(os.Getenv("TINYSTORE_BLOBS_FILES")); err == nil && count > 0 {
		return count
	}
	return 1_000_000
}

// blobsBytes are size random bytes from a seeded source, as media is: nothing
// a codec or a disk could compress
func blobsBytes(seed uint64, size int) []byte {
	random := rand.NewChaCha8([32]byte{byte(seed), byte(seed >> 8), byte(seed >> 16), byte(seed >> 24)})
	data := make([]byte, size)
	_, _ = random.Read(data)
	return data
}

// blobsName is a content's file under objects/: a directory for each 4,096
// ids, so that none holds more names than that
//
//	0x1a3f07 → objects/1a3/1a3f07
func blobsName(id int64) (dir, name string) {
	dir = strconv.FormatInt(id>>12, 16)
	return filepath.Join("objects", dir), filepath.Join("objects", dir, strconv.FormatInt(id, 16))
}

// blobsTree creates the directories of a content's file once each
type blobsTree struct {
	root string
	made sync.Map
}

func (b *blobsTree) dirOf(id int64) (string, error) {
	dir, _ := blobsName(id)
	full := filepath.Join(b.root, dir)
	if _, done := b.made.Load(dir); done {
		return full, nil
	}
	if err := os.MkdirAll(full, 0o750); err != nil {
		return "", err
	}
	b.made.Store(dir, true)
	return full, nil
}

// blobsWrite writes a content's file the way the engine would: created, its
// bytes hashed as they go, synced; its directory is the caller's to sync
func blobsWrite(path string, data []byte, sum hash.Hash) error {
	file, err := os.OpenFile(path, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o640)
	if err != nil {
		return err
	}
	_, err = io.MultiWriter(file, sum).Write(data)
	if err == nil {
		err = file.Sync()
	}
	return errorsJoin(err, file.Close())
}

func errorsJoin(errs ...error) error {
	for _, err := range errs {
		if err != nil {
			return err
		}
	}
	return nil
}

// blobsLatencies is one load's calls: how many, and how long each took
type blobsLatencies struct {
	mu      sync.Mutex
	took    []time.Duration
	elapsed time.Duration
}

func (l *blobsLatencies) add(took time.Duration) {
	l.mu.Lock()
	l.took = append(l.took, took)
	l.mu.Unlock()
}

func (l *blobsLatencies) sorted() []time.Duration {
	took := slices.Clone(l.took)
	slices.Sort(took)
	return took
}

func (l *blobsLatencies) String() string {
	took := l.sorted()
	if len(took) == 0 {
		return "no calls"
	}
	return fmt.Sprintf("%8.0f a second  p50 %s p99 %s", float64(len(took))/l.elapsed.Seconds(),
		blobsMicros(took[len(took)/2]), blobsMicros(took[len(took)*99/100]))
}

// blobsLoad calls call from callers goroutines, each with its own counter,
// until length has passed, and times each call
func blobsLoad(t *testing.T, callers int, length time.Duration, call func(caller, n int) error) *blobsLatencies {
	t.Helper()
	latencies := &blobsLatencies{}
	started := time.Now()
	deadline := started.Add(length)
	errs := make([]error, callers)
	var running sync.WaitGroup
	for caller := range callers {
		running.Go(func() {
			for n := 0; time.Now().Before(deadline) && errs[caller] == nil; n++ {
				began := time.Now()
				errs[caller] = call(caller, n)
				latencies.add(time.Since(began))
			}
		})
	}
	running.Wait()
	latencies.elapsed = time.Since(started)
	for _, err := range errs {
		if err != nil {
			t.Fatal(err)
		}
	}
	return latencies
}

// blobsMicros is a duration in microseconds, as the probe reported them
func blobsMicros(d time.Duration) string {
	return fmt.Sprintf("%7.0f µs", float64(d)/float64(time.Microsecond))
}

func blobsSize(size int) string {
	switch {
	case size >= 1<<30 && size%(1<<30) == 0:
		return fmt.Sprintf("%d GiB", size>>30)
	case size >= 1<<20 && size%(1<<20) == 0:
		return fmt.Sprintf("%d MiB", size>>20)
	}
	return fmt.Sprintf("%d KiB", size>>10)
}

// blobsHash is the hash every content keeps
func blobsHash() hash.Hash { return sha256.New() }
