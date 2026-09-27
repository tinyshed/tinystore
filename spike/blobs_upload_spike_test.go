package spike

import (
	"bufio"
	"context"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// blobsLarge is the size of the large upload, 4 GiB unless TINYSTORE_BLOBS_LARGE
// says in MiB
func blobsLarge() int64 {
	if mib, err := strconv.Atoi(os.Getenv("TINYSTORE_BLOBS_LARGE")); err == nil && mib > 0 {
		return int64(mib) << 20
	}
	return 4 << 30
}

// TestBlobsLargeUpload writes one large upload in 64 KiB writes, synced once
// at its end, every 256 MiB or every 64 MiB, and reports the whole write, the
// last sync a Commit waits for, and the most dirty memory the system held
func TestBlobsLargeUpload(t *testing.T) {
	kvMeasuring(t)
	size := blobsLarge()
	chunk := blobsBytes(64, 64<<10)
	for _, every := range []int64{0, 256 << 20, 64 << 20} {
		path := filepath.Join(blobsDir(t), "upload")
		dirty := blobsWatchDirty()
		began := time.Now()
		file, err := os.Create(path)
		if err != nil {
			t.Fatal(err)
		}
		hash := blobsHash()
		for written := int64(0); written < size && err == nil; written += int64(len(chunk)) {
			_, err = file.Write(chunk)
			_, _ = hash.Write(chunk)
			if err == nil && every > 0 && (written+int64(len(chunk)))%every == 0 {
				err = file.Sync()
			}
		}
		written := time.Since(began)
		err = errorsJoin(err, file.Sync())
		last := time.Since(began) - written
		err = errorsJoin(err, file.Close())
		if err != nil {
			t.Fatal(err)
		}
		t.Logf("%s synced %-13s  written in %s, %5.2f GB/s; the last sync %s; dirty at most %s", blobsSize(int(size)),
			blobsEvery(every), jobsMillis(written), float64(size)/written.Seconds()/1e9, jobsMillis(last), dirty())
		_ = os.Remove(path)
	}
}

func blobsEvery(every int64) string {
	if every == 0 {
		return "at its end"
	}
	return "every " + blobsSize(int(every))
}

// blobsWatchDirty samples the system's dirty memory every 50 ms, on Linux,
// until the returned function answers the most it saw
func blobsWatchDirty() func() string {
	var peak atomic.Int64
	peak.Store(-1)
	stop, done := make(chan struct{}), make(chan struct{})
	go func() {
		defer close(done)
		for {
			if kib := blobsDirtyKiB(); kib > peak.Load() {
				peak.Store(kib)
			}
			select {
			case <-stop:
				return
			case <-time.After(50 * time.Millisecond):
			}
		}
	}()
	return func() string {
		close(stop)
		<-done
		if peak.Load() < 0 {
			return "unknown here"
		}
		return fmt.Sprintf("%.0f MiB", float64(peak.Load())/1024)
	}
}

// blobsDirtyKiB reads Dirty from /proc/meminfo, or -1 where there is none
func blobsDirtyKiB() int64 {
	file, err := os.Open("/proc/meminfo")
	if err != nil {
		return -1
	}
	defer func() { _ = file.Close() }()
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		if fields := strings.Fields(scanner.Text()); len(fields) >= 2 && fields[0] == "Dirty:" {
			kib, _ := strconv.ParseInt(fields[1], 10, 64)
			return kib
		}
	}
	return -1
}

// blobsStream yields size bytes, a 64 KiB chunk again and again, so that an
// upload's bytes need no memory of their own
type blobsStream struct {
	chunk []byte
	left  int64
}

func (s *blobsStream) Read(p []byte) (int, error) {
	if s.left == 0 {
		return 0, io.EOF
	}
	n := copy(p[:min(int64(len(p)), s.left)], s.chunk)
	s.left -= int64(n)
	return n, nil
}

// TestBlobsUploadsAtOnce runs 16 to 1024 uploads of 1 MiB at once, and 16
// and 64 of 64 MiB, each streamed through a buffer of the inline size into a
// file of its own, synced with its directory and committed by a row, and
// reports their rate, the heap's peak and the files open at once
func TestBlobsUploadsAtOnce(t *testing.T) {
	kvMeasuring(t)
	for _, load := range []struct{ uploads, size int }{
		{16, 1 << 20}, {64, 1 << 20}, {256, 1 << 20}, {1024, 1 << 20}, {16, 64 << 20}, {64, 64 << 20},
	} {
		store := blobsOpenStore(t)
		heap := blobsWatchHeap()
		var open, mostOpen atomic.Int64
		count := max(load.uploads, (4<<30)/load.size) // 4 GiB, and each upload's slot used once at the least
		began := time.Now()
		blobsEach(t, load.uploads, count, func(int) error {
			return blobsUpload(t.Context(), store, int64(load.size), &open, &mostOpen)
		})
		elapsed := time.Since(began)
		t.Logf("%4d uploads of %6s at once  %6.0f a second, %5.2f GB/s; heap at most %s; files open at most %d",
			load.uploads, blobsSize(load.size), float64(count)/elapsed.Seconds(),
			float64(count)*float64(load.size)/elapsed.Seconds()/1e9, heap(), mostOpen.Load())
	}
}

// blobsUpload streams one object into a file through a 64 KiB buffer, hashing
// it, syncs the file and its directory, and commits the object's row
func blobsUpload(ctx context.Context, store *blobsStore, size int64, open, mostOpen *atomic.Int64) error {
	id := store.nextID.Add(1)
	dir, err := store.tree.dirOf(id)
	if err != nil {
		return err
	}
	_, name := blobsName(id)
	path := filepath.Join(store.tree.root, name)
	file, err := os.OpenFile(path, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o640)
	if err != nil {
		return err
	}
	blobsRaise(mostOpen, open.Add(1))
	hash := blobsHash()
	buffer := make([]byte, 64<<10)
	_, err = io.CopyBuffer(io.MultiWriter(file, hash), &blobsStream{chunk: blobsChunk, left: size}, buffer)
	err = errorsJoin(err, file.Sync(), file.Close())
	open.Add(-1)
	if err == nil {
		err = store.syncName(dir, blobsFileWay{shareSync: true})
	}
	if err != nil {
		return err
	}
	return store.db.UpdateGrouped(ctx, 0, func(w sqlite.Writer) error {
		_, err := w.ExecContext(ctx, blobsInsertObject, fmt.Sprint("u/", id), id, size, hash.Sum(nil))
		return err
	})
}

var blobsChunk = blobsBytes(99, 64<<10)

// blobsRaise keeps the largest value most has been given
func blobsRaise(most *atomic.Int64, value int64) {
	for seen := most.Load(); value > seen && !most.CompareAndSwap(seen, value); seen = most.Load() {
	}
}

// blobsWatchHeap collects what earlier work left, then samples the Go heap in
// use every 20 ms until the returned function answers the most it saw
func blobsWatchHeap() func() string {
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
