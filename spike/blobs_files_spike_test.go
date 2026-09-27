package spike

import (
	"archive/zip"
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

// TestBlobsManyFiles creates a million files of 8 KiB from 64 writers, each
// synced with its directory's sync shared, and reports the rate as the
// directories fill, the disk they take, and what a snapshot, a backup and a
// restore of them cost against copying their bytes as one file
func TestBlobsManyFiles(t *testing.T) {
	kvMeasuring(t)
	count := blobsFileCount()
	const size, writers = 8 << 10, 64
	store := blobsOpenStore(t)
	data := blobsBytes(8, size)
	var next atomic.Int64
	slice := max(count/10, 1)
	began, last := time.Now(), time.Now()
	var mu sync.Mutex
	blobsEach(t, writers, count, func(int) error {
		id := next.Add(1)
		if _, err := store.writeFile(id, data, blobsFileWay{shareSync: true}); err != nil {
			return err
		}
		if id%int64(slice) == 0 {
			mu.Lock()
			t.Logf("files %8d  the last %d at %8.0f a second", id, slice, float64(slice)/time.Since(last).Seconds())
			last = time.Now()
			mu.Unlock()
		}
		return nil
	})
	objects := filepath.Join(store.tree.root, "objects")
	t.Logf("files %d of %s from %d writers in %s, %8.0f a second, disk %s for %.1f MiB of bytes", count,
		blobsSize(size), writers, jobsMillis(time.Since(began)), float64(count)/time.Since(began).Seconds(),
		blobsDiskUsed(objects), float64(count*size)/(1<<20))
	blobsLinkAll(t, objects, filepath.Join(store.tree.root, "snapshot"))
	blobsCopyAsOne(t, objects, filepath.Join(store.tree.root, "one.bin"))
	archive := filepath.Join(store.tree.root, "backup.zip")
	blobsZip(t, objects, archive, zip.Store)
	blobsRestore(t, archive, filepath.Join(store.tree.root, "restored"))
}

// blobsEach calls call count times in all from writers goroutines
func blobsEach(t *testing.T, writers, count int, call func(n int) error) {
	t.Helper()
	var next atomic.Int64
	errs := make([]error, writers)
	var running sync.WaitGroup
	for writer := range writers {
		running.Go(func() {
			for n := int(next.Add(1)); n <= count && errs[writer] == nil; n = int(next.Add(1)) {
				errs[writer] = call(n)
			}
		})
	}
	running.Wait()
	if err := errors.Join(errs...); err != nil {
		t.Fatal(err)
	}
}

// blobsDiskUsed is what du says a tree takes, where du exists
func blobsDiskUsed(dir string) string {
	output, err := exec.CommandContext(context.Background(), "du", "-sk", dir).Output()
	if err != nil {
		return "unknown (no du)"
	}
	kib, err := strconv.ParseInt(strings.Fields(string(output))[0], 10, 64)
	if err != nil {
		return "unknown"
	}
	return fmt.Sprintf("%.1f MiB", float64(kib)/1024)
}

// blobsLinkAll hard-links every file of a tree into another, as a snapshot
// would, and times it
func blobsLinkAll(t *testing.T, from, into string) {
	t.Helper()
	began := time.Now()
	linked := 0
	err := filepath.WalkDir(from, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		target := filepath.Join(into, strings.TrimPrefix(path, from))
		if entry.IsDir() {
			return os.MkdirAll(target, 0o750)
		}
		linked++
		return os.Link(path, target)
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("snapshot: linked %d files in %s, %8.0f a second", linked, jobsMillis(time.Since(began)),
		float64(linked)/time.Since(began).Seconds())
}

// blobsCopyAsOne copies every file's bytes into one file and syncs it: what a
// snapshot that copied the bytes would cost at the least
func blobsCopyAsOne(t *testing.T, from, into string) {
	t.Helper()
	began := time.Now()
	out, err := os.Create(into)
	if err != nil {
		t.Fatal(err)
	}
	var copied int64
	err = filepath.WalkDir(from, func(path string, entry fs.DirEntry, err error) error {
		if err != nil || entry.IsDir() {
			return err
		}
		n, err := blobsAppendFile(out, path)
		copied += n
		return err
	})
	err = errorsJoin(err, out.Sync(), out.Close())
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("the same bytes copied into one file: %.1f MiB in %s, %5.2f GB/s", float64(copied)/(1<<20),
		jobsMillis(time.Since(began)), float64(copied)/time.Since(began).Seconds()/1e9)
}

func blobsAppendFile(out io.Writer, path string) (int64, error) {
	in, err := os.Open(path)
	if err != nil {
		return 0, err
	}
	n, err := io.Copy(out, in)
	return n, errorsJoin(err, in.Close())
}

// blobsZip writes every file of a tree into one zip, stored or deflated, and
// times it
func blobsZip(t *testing.T, from, archive string, method uint16) {
	t.Helper()
	began := time.Now()
	out, err := os.Create(archive)
	if err != nil {
		t.Fatal(err)
	}
	writer := zip.NewWriter(out)
	entries := 0
	err = filepath.WalkDir(from, func(path string, entry fs.DirEntry, err error) error {
		if err != nil || entry.IsDir() {
			return err
		}
		entries++
		name := filepath.ToSlash(strings.TrimPrefix(path, from))
		w, err := writer.CreateHeader(&zip.FileHeader{Name: strings.TrimPrefix(name, "/"), Method: method})
		if err != nil {
			return err
		}
		_, err = blobsAppendFile(w, path)
		return err
	})
	err = errorsJoin(err, writer.Close(), out.Close())
	if err != nil {
		t.Fatal(err)
	}
	info, err := os.Stat(archive)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("backup: %d entries, %s, into %.1f MiB in %s", entries, blobsMethod(method),
		float64(info.Size())/(1<<20), jobsMillis(time.Since(began)))
}

func blobsMethod(method uint16) string {
	if method == zip.Store {
		return "stored"
	}
	return "deflated"
}

// blobsRestore opens a zip, reports the heap its directory holds, and writes
// every entry back out
func blobsRestore(t *testing.T, archive, into string) {
	t.Helper()
	var before, after runtime.MemStats
	runtime.GC()
	runtime.ReadMemStats(&before)
	began := time.Now()
	reader, err := zip.OpenReader(archive)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = reader.Close() }()
	runtime.GC()
	runtime.ReadMemStats(&after)
	t.Logf("restore: the zip's directory of %d entries opened in %s, holding %.1f MiB of heap", len(reader.File),
		jobsMillis(time.Since(began)), float64(after.HeapAlloc-min(before.HeapAlloc, after.HeapAlloc))/(1<<20))
	began = time.Now()
	for _, entry := range reader.File {
		if err = blobsExtract(entry, filepath.Join(into, filepath.FromSlash(entry.Name))); err != nil {
			t.Fatal(err)
		}
	}
	t.Logf("restore: %d entries written back in %s", len(reader.File), jobsMillis(time.Since(began)))
}

func blobsExtract(entry *zip.File, target string) error {
	if err := os.MkdirAll(filepath.Dir(target), 0o750); err != nil {
		return err
	}
	in, err := entry.Open()
	if err != nil {
		return err
	}
	out, err := os.Create(target)
	if err != nil {
		return errorsJoin(err, in.Close())
	}
	_, err = io.Copy(out, in)
	return errorsJoin(err, out.Close(), in.Close())
}

// blobsPack is one file that objects are appended to, synced for a batch of
// appends at a time: an append waits for a sync that began after it
type blobsPack struct {
	mu     sync.Mutex
	file   *os.File
	offset int64
	syncer blobsFileSyncer
}

func (p *blobsPack) append(data []byte) error {
	p.mu.Lock()
	_, err := p.file.WriteAt(data, p.offset)
	p.offset += int64(len(data))
	p.mu.Unlock()
	if err != nil {
		return err
	}
	return p.syncer.sync(p.file)
}

// blobsFileSyncer shares a file's sync as blobsDirSyncer shares a directory's
type blobsFileSyncer struct {
	blobsDirSyncer
}

func (s *blobsFileSyncer) sync(file *os.File) error {
	return s.run(file.Sync)
}

// TestBlobsWriters writes objects of 64 KiB to 1 MiB from 1, 8 and 128
// writers, each a file synced with its directory, and appended to one pack
// that a batch of appends syncs once
func TestBlobsWriters(t *testing.T) {
	kvMeasuring(t)
	for _, size := range []int{64 << 10, 256 << 10, 1 << 20} {
		data := blobsBytes(uint64(size), size)
		for _, writers := range []int{1, 8, 128} {
			store := blobsOpenStore(t)
			files := blobsLoad(t, writers, blobsSeconds(), func(int, int) error {
				_, err := store.writeFile(store.nextID.Add(1), data, blobsFileWay{shareSync: true})
				return err
			})
			pack := blobsOpenPack(t, store.tree.root)
			packed := blobsLoad(t, writers, blobsSeconds(), func(int, int) error { return pack.append(data) })
			t.Logf("%7s %3d writers  files %s", blobsSize(size), writers, files)
			t.Logf("%7s %3d writers  pack  %s", blobsSize(size), writers, packed)
		}
	}
}

func blobsOpenPack(t *testing.T, dir string) *blobsPack {
	t.Helper()
	file, err := os.Create(filepath.Join(dir, "000001.pack"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = file.Close() })
	return &blobsPack{file: file}
}

// TestBlobsOneWriter repeats the probe's table: one writer, 500 objects a
// size, a file synced with its directory, a pack synced after each append, and
// a pack synced after every 32
func TestBlobsOneWriter(t *testing.T) {
	kvMeasuring(t)
	for _, size := range []int{4 << 10, 64 << 10, 256 << 10, 1 << 20} {
		data := blobsBytes(uint64(size), size)
		store := blobsOpenStore(t)
		files := blobsTimes(t, 500, func(int) error {
			_, err := store.writeFile(store.nextID.Add(1), data, blobsFileWay{syncDir: true})
			return err
		})
		pack := blobsOpenPack(t, store.tree.root)
		each := blobsTimes(t, 500, func(int) error {
			_, err := pack.file.Write(data)
			return errorsJoin(err, pack.file.Sync())
		})
		batched := blobsTimes(t, 500, func(n int) error {
			_, err := pack.file.Write(data)
			if err == nil && n%32 == 31 {
				err = pack.file.Sync()
			}
			return err
		})
		t.Logf("%7s one writer  file %s  pack synced each %s  per 32 %s", blobsSize(size), files, each, batched)
	}
}

// blobsTimes calls call count times and answers the mean call, which spreads
// a sync shared by a batch over the objects of the batch
func blobsTimes(t *testing.T, count int, call func(n int) error) string {
	t.Helper()
	began := time.Now()
	for n := range count {
		if err := call(n); err != nil {
			t.Fatal(err)
		}
	}
	return blobsMicros(time.Since(began) / time.Duration(count))
}
