package blobs

import (
	"bytes"
	"context"
	"errors"
	"io"
	"runtime"
	"strconv"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// an object appears at its commit, whole, or not at all: before its Commit no
// call sees it, and a reader racing its replacements reads one whole version
func TestAnObjectAppearsWholeAtItsCommitOrNotAtAll(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	upload, err := media.Create(t.Context(), "film")
	if err != nil {
		t.Fatal(err)
	}
	if _, err = upload.Write(randomBytes(1, 1<<20)); err != nil {
		t.Fatal(err)
	}
	mustBeAbsent(t, media, "film")
	if usage, usageErr := media.Usage(t.Context()); usageErr != nil || usage != (Usage{}) {
		t.Fatalf("an upload before its commit counts %+v, %v", usage, usageErr)
	}
	if _, err = upload.Commit(t.Context()); err != nil {
		t.Fatal(err)
	}

	versions := map[string][]byte{}
	for i, size := range []int{10, inlineSize, 300 << 10, 1 << 20} {
		data := randomBytes(uint64(100+i), size)
		versions[mustPut(t, media, "film", data).ETag] = data
	}
	var readers sync.WaitGroup
	stop := make(chan struct{})
	for range 4 {
		readers.Go(func() { readWholeVersions(t, media, "film", versions, stop) })
	}
	for round := range 50 {
		for _, data := range versions {
			if _, err = media.Put(t.Context(), "film", bytes.NewReader(data)); err != nil {
				t.Fatal(round, err)
			}
		}
	}
	close(stop)
	readers.Wait()
	last, _, _ := media.Stat(t.Context(), "film")
	s.maintain(t)
	if last.Size > inlineSize {
		s.mustHoldFiles(t, 1)
	} else {
		s.mustHoldFiles(t, 0)
	}
}

// readWholeVersions opens key until stop, and fails the test on a read that is
// not one of the versions written whole, under its own ETag
func readWholeVersions(t *testing.T, bucket *Bucket, key string, versions map[string][]byte, stop chan struct{}) {
	for {
		select {
		case <-stop:
			return
		default:
		}
		reader, found, err := bucket.Open(t.Context(), key)
		if err != nil || !found {
			t.Errorf("Open while replacing: %v, %v", found, err)
			return
		}
		data, err := io.ReadAll(reader)
		_ = reader.Close()
		if err != nil || !bytes.Equal(data, versions[reader.ETag]) {
			t.Errorf("read %d bytes under %s: %v", len(data), reader.ETag, err)
			return
		}
	}
}

// an upload aborted, ended by its context, by a failing stream or by the
// store's Close leaves nothing: no row, no file, no id held
func TestAnAbortedOrAbandonedUploadLeavesNothing(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	for _, size := range []int{100, 1 << 20} {
		upload, err := media.Create(t.Context(), "aborted")
		if err != nil {
			t.Fatal(err)
		}
		if _, err = upload.Write(randomBytes(1, size)); err != nil {
			t.Fatal(err)
		}
		upload.Abort()
		if _, err = upload.Commit(t.Context()); !errors.Is(err, tinystore.ErrClosed) {
			t.Fatalf("a Commit after Abort: %v", err)
		}

		ended, cancel := context.WithCancel(t.Context())
		upload, err = media.Create(ended, "abandoned")
		if err != nil {
			t.Fatal(err)
		}
		if _, err = upload.Write(randomBytes(2, size)); err != nil {
			t.Fatal(err)
		}
		cancel()
		if _, err = upload.Write([]byte("more")); !errors.Is(err, context.Canceled) {
			t.Fatalf("a Write after its context ended: %v", err)
		}

		broken := io.MultiReader(bytes.NewReader(randomBytes(3, size)), failingReader{})
		if _, err = media.Put(t.Context(), "broken", broken); !errors.Is(err, errBrokenStream) {
			t.Fatalf("a Put of a stream that fails: %v", err)
		}
	}
	for _, key := range []string{"aborted", "abandoned", "broken"} {
		mustBeAbsent(t, media, key)
	}
	s.mustHoldFiles(t, 0)
	if mark := s.settledMark(); mark != s.ids.last {
		t.Fatalf("ids are held after every upload ended: the mark is %d, the last id %d", mark, s.ids.last)
	}

	upload, err := media.Create(t.Context(), "closed")
	if err != nil {
		t.Fatal(err)
	}
	if _, err = upload.Write(randomBytes(4, 1<<20)); err != nil {
		t.Fatal(err)
	}
	if err = s.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	if _, err = upload.Write([]byte("more")); !errors.Is(err, tinystore.ErrClosed) {
		t.Fatalf("a Write after the store closed: %v", err)
	}
	s = openTestStore(t, s.dir)
	s.mustHoldFiles(t, 0)
}

var errBrokenStream = errors.New("the connection dropped")

type failingReader struct{}

func (failingReader) Read([]byte) (int, error) { return 0, errBrokenStream }

// a stream shorter or longer than its declared Size is refused and leaves nothing
func TestAStreamThatDisagreesWithItsSizeIsRefused(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	for _, size := range []int{10, inlineSize, 1 << 20} {
		data := randomBytes(5, size)
		stream := io.MultiReader(bytes.NewReader(data))
		if _, err := media.Put(t.Context(), "short", stream, Size(int64(size+1))); !errors.Is(err, tinystore.ErrInvalid) {
			t.Fatalf("a stream of %d bytes, one short of its Size: %v", size, err)
		}
		stream = io.MultiReader(bytes.NewReader(data))
		if _, err := media.Put(t.Context(), "long", stream, Size(int64(size-1))); !errors.Is(err, tinystore.ErrInvalid) {
			t.Fatalf("a stream of %d bytes, one past its Size: %v", size, err)
		}
		if _, err := media.Put(t.Context(), "said", bytes.NewReader(data), Size(-1)); err != nil {
			t.Fatalf("a negative Size says nothing: %v", err)
		}
	}
	mustBeAbsent(t, media, "short")
	mustBeAbsent(t, media, "long")
	s.mustHoldFiles(t, 1)
}

// an upload past its bucket's MaxSize, or one that would leave the disk less
// free than KeepFree, stops and leaves nothing
func TestAnUploadPastItsBoundsStopsAndLeavesNothing(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	small := openTestBucket(t, s, "small", MaxSize(1<<20))
	for _, size := range []int{1<<20 + 1, 2 << 20} {
		stream := io.MultiReader(bytes.NewReader(randomBytes(6, size)))
		if _, err := small.Put(t.Context(), "large", stream); !errors.Is(err, tinystore.ErrLimit) {
			t.Fatalf("a stream of %d bytes past MaxSize: %v", size, err)
		}
		if _, err := small.Create(t.Context(), "large", Size(int64(size))); !errors.Is(err, tinystore.ErrLimit) {
			t.Fatalf("a declared size of %d bytes past MaxSize: %v", size, err)
		}
	}
	mustPut(t, small, "fits", randomBytes(6, 1<<20))
	s.mustHoldFiles(t, 1)

	if _, err := freeSpace(t.TempDir()); err != nil {
		t.Skipf("this system does not say what its disk has free: %v", err)
	}
	full := openTestStoreWith(t, t.TempDir(), tinystore.Options{}, Options{KeepFree: 1 << 62})
	media := openTestBucket(t, full, "media")
	if _, err := media.Put(t.Context(), "inline", bytes.NewReader(randomBytes(7, 100))); err != nil {
		t.Fatalf("an inline object waits on no disk: %v", err)
	}
	stream := io.MultiReader(bytes.NewReader(randomBytes(7, 1<<20)))
	if _, err := media.Put(t.Context(), "film", stream); !errors.Is(err, tinystore.ErrLimit) {
		t.Fatalf("an upload past KeepFree: %v", err)
	}
	if _, err := media.Create(t.Context(), "film", Size(1<<20)); !errors.Is(err, tinystore.ErrLimit) {
		t.Fatalf("a declared size past KeepFree: %v", err)
	}
	full.mustHoldFiles(t, 0)
}

// endless yields a chunk again and again up to its length, so that a large
// upload's bytes take no memory of the test's own
type endless struct {
	chunk  []byte
	left   int64
	offset int
}

func (e *endless) Read(p []byte) (int, error) {
	if e.left == 0 {
		return 0, io.EOF
	}
	n := int(min(int64(len(p)), e.left))
	for copied := 0; copied < n; {
		read := copy(p[copied:n], e.chunk[e.offset:])
		copied += read
		e.offset = (e.offset + read) % len(e.chunk)
	}
	e.left -= int64(n)
	return n, nil
}

func TestTheStreamingFixtureKeepsItsBytesAtEveryReadSize(t *testing.T) {
	chunk := randomBytes(55, 64<<10)
	want := bytes.Repeat(chunk, 3)
	for _, width := range []int{7, 16 << 10, 64 << 10, 128 << 10} {
		reader := &endless{chunk: chunk, left: int64(len(want))}
		var got bytes.Buffer
		buffer := make([]byte, width)
		for {
			n, err := reader.Read(buffer)
			got.Write(buffer[:n])
			if errors.Is(err, io.EOF) {
				break
			}
			if err != nil {
				t.Fatal(err)
			}
		}
		if !bytes.Equal(got.Bytes(), want) {
			t.Fatalf("fixture differs with a %d-byte read", width)
		}
	}
}

type observedStream struct {
	io.Reader
	largest int
}

func (r *observedStream) Read(p []byte) (int, error) {
	r.largest = max(r.largest, len(p))
	return r.Reader.Read(p)
}

func TestStreamingUsesOnlyTheBufferItsBudgetCanHold(t *testing.T) {
	data := randomBytes(5, 1<<20)
	for _, budget := range []int64{inlineSize, 64 << 10, 80 << 10} {
		s := openTestStoreWith(t, t.TempDir(), tinystore.Options{Memory: budget}, Options{})
		media := openTestBucket(t, s, "media")
		reader := &observedStream{Reader: bytes.NewReader(data)}
		if _, err := media.Put(t.Context(), "stream", reader); err != nil {
			t.Fatal(err)
		}
		want := inlineSize
		if budget >= 80<<10 {
			want = 64 << 10
		}
		if reader.largest != want {
			t.Fatalf("budget %d: largest read %d, want %d", budget, reader.largest, want)
		}
		if usage := s.runtime.Memory(); usage.Used != 0 || usage.Peak > budget {
			t.Fatalf("budget %d: %+v", budget, usage)
		}
		mustHold(t, media, "stream", data)
	}
}

type failedStream struct{ err error }

func (r failedStream) Read([]byte) (int, error) { return 0, r.err }

func TestAFailedStreamReleasesItsLargerBuffer(t *testing.T) {
	s := openTestStoreWith(t, t.TempDir(), tinystore.Options{Memory: 80 << 10}, Options{})
	media := openTestBucket(t, s, "media")
	failure := errors.New("the source stopped")
	reader := io.MultiReader(bytes.NewReader(randomBytes(6, 512<<10)), failedStream{err: failure})
	if _, err := media.Put(t.Context(), "stream", reader); !errors.Is(err, failure) {
		t.Fatalf("failed source: %v", err)
	}
	if usage := s.runtime.Memory(); usage.Used != 0 || usage.Peak != 80<<10 {
		t.Fatalf("failed stream memory: %+v", usage)
	}
	mustBeAbsent(t, media, "stream")
	s.mustHoldFiles(t, 0)
	if left := s.filesIn(t, uploadsDir); len(left) != 0 {
		t.Fatalf("failed stream left uploads: %v", left)
	}
}

// Memory does not follow an object's size: with a store whose memory is the
// inline size, an upload of 64 MiB fits it and allocates next to nothing.
func TestMemoryDoesNotGrowWithAnObjectsSize(t *testing.T) {
	s := openTestStoreWith(t, t.TempDir(), tinystore.Options{Memory: inlineSize}, Options{})
	media := openTestBucket(t, s, "media")
	const size = 64 << 20
	var before, after runtime.MemStats
	runtime.GC()
	runtime.ReadMemStats(&before)
	object, err := media.Put(t.Context(), "film", &endless{chunk: randomBytes(8, 64<<10), left: size})
	runtime.ReadMemStats(&after)
	if err != nil || object.Size != size {
		t.Fatalf("Put of 64 MiB: %+v, %v", object, err)
	}
	if allocated := after.TotalAlloc - before.TotalAlloc; allocated > 4<<20 {
		t.Fatalf("an upload of 64 MiB allocated %d bytes", allocated)
	}
	if usage := s.runtime.Memory(); usage.Peak > inlineSize || usage.Used != 0 {
		t.Fatalf("the store's memory: %+v", usage)
	}
}

// uploads wait for a slot and for the store's memory, and leave when their
// context ends
func TestUploadsWaitForTheirSlotsAndMemory(t *testing.T) {
	s := openTestStoreWith(t, t.TempDir(), tinystore.Options{Memory: inlineSize}, Options{})
	media := openTestBucket(t, s, "media")
	first, err := media.Create(t.Context(), "first")
	if err != nil {
		t.Fatal(err)
	}
	short, cancel := context.WithTimeout(t.Context(), 50*time.Millisecond)
	defer cancel()
	if _, err = media.Create(short, "second"); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("an upload beside one that holds the store's memory: %v", err)
	}
	waited := make(chan error, 1)
	go func() {
		_, putErr := media.Put(t.Context(), "second", bytes.NewReader([]byte("waited")))
		waited <- putErr
	}()
	if _, err = first.Commit(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err = <-waited; err != nil {
		t.Fatalf("the upload that waited: %v", err)
	}

	s.uploads = make(chan struct{}, 1)
	third, err := media.Create(t.Context(), "third")
	if err != nil {
		t.Fatal(err)
	}
	defer third.Abort()
	short, cancel = context.WithTimeout(t.Context(), 50*time.Millisecond)
	defer cancel()
	if _, err = media.Put(short, "fourth", bytes.NewReader(nil)); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("an upload beside one that holds the only slot: %v", err)
	}
}

// IfNoneMatch creates only: a retry adds nothing, and a key deleted or
// expired is free again
func TestIfNoneMatchCreatesOnce(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	for _, size := range []int{10, 1 << 20} {
		key := "once/" + strconv.Itoa(size)
		first := mustPut(t, media, key, randomBytes(9, size), IfNoneMatch(), TTL(time.Hour))
		_, err := media.Put(t.Context(), key, bytes.NewReader(randomBytes(10, size)), IfNoneMatch())
		if !errors.Is(err, tinystore.ErrConflict) {
			t.Fatalf("a second create of %d bytes: %v", size, err)
		}
		if _, err = media.Create(t.Context(), key, IfNoneMatch()); !errors.Is(err, tinystore.ErrConflict) {
			t.Fatalf("a create that begins over a live object: %v", err)
		}
		if _, object, _ := readAll(t, media, key); object.ETag != first.ETag {
			t.Fatalf("the retry replaced the first: %s, want %s", object.ETag, first.ETag)
		}
	}
	s.clock.advance(2 * time.Hour)
	mustPut(t, media, "once/10", randomBytes(11, 10), IfNoneMatch())
	if err := media.Delete(t.Context(), "once/1048576"); err != nil {
		t.Fatal(err)
	}
	mustPut(t, media, "once/1048576", randomBytes(11, 1<<20), IfNoneMatch())
	s.maintain(t)
	s.mustHoldFiles(t, 1)
}

// Of two replaces conditional on the ETag both read, one goes through and the
// other conflicts and leaves nothing, inline or in a file. It holds whether the
// second began before the first committed or after.
func TestOneOfTwoConditionalReplacesConflicts(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	for _, size := range []int{10, 1 << 20} {
		read := mustPut(t, media, "doc", randomBytes(12, size))
		var won, lost atomic.Int32
		var editors sync.WaitGroup
		for editor := range 8 {
			editors.Go(func() {
				data := randomBytes(uint64(20+editor), size)
				_, err := media.Put(t.Context(), "doc", bytes.NewReader(data), IfMatch(read.ETag))
				switch {
				case err == nil:
					won.Add(1)
				case errors.Is(err, tinystore.ErrConflict):
					lost.Add(1)
				default:
					t.Error(err)
				}
			})
		}
		editors.Wait()
		if won.Load() != 1 || lost.Load() != 7 {
			t.Fatalf("of eight saves over %d bytes %d went through and %d conflicted", size, won.Load(), lost.Load())
		}

		now, _, _ := media.Stat(t.Context(), "doc")
		late, err := media.Create(t.Context(), "doc", IfMatch(now.ETag))
		if err != nil {
			t.Fatal(err)
		}
		mustPut(t, media, "doc", randomBytes(30, size), IfMatch(now.ETag))
		if _, err = late.Write(randomBytes(31, size)); err != nil {
			t.Fatal(err)
		}
		if _, err = late.Commit(t.Context()); !errors.Is(err, tinystore.ErrConflict) {
			t.Fatalf("a save that began before another committed: %v", err)
		}
	}
	for _, list := range []string{`"nothing", *`, "*"} {
		mustPut(t, media, "doc", randomBytes(40, 10), IfMatch(list))
	}
	if _, err := media.Put(t.Context(), "absent", bytes.NewReader(nil), IfMatch("*")); !errors.Is(err, tinystore.ErrConflict) {
		t.Fatalf("IfMatch(*) over an absent key: %v", err)
	}
	s.mustHoldFiles(t, 0)
}

// every call that holds bytes waits for the store's memory before it makes
// them, and gives back all it held
func TestStoreMemoryBoundsUploadsReadsAndScans(t *testing.T) {
	budget := int64((scanLimit + 1) * objectHeld)
	s := openTestStoreWith(t, t.TempDir(), tinystore.Options{Memory: budget}, Options{})
	media := openTestBucket(t, s, "media")
	mustPut(t, media, "kept", randomBytes(1, 100))
	calls := map[string]func(context.Context) error{
		"Put": func(ctx context.Context) error {
			_, err := media.Put(ctx, "put", bytes.NewReader([]byte("value")))
			return err
		},
		"Open": func(ctx context.Context) error {
			reader, _, err := media.Open(ctx, "kept")
			if err == nil {
				err = reader.Close()
			}
			return err
		},
		"Scan": func(ctx context.Context) error {
			_, err := media.Scan(ctx, Query{})
			return err
		},
	}
	for _, name := range []string{"Put", "Open", "Scan"} {
		taken, err := s.runtime.Reserve(t.Context(), budget)
		if err != nil {
			t.Fatal(err)
		}
		short, cancel := context.WithTimeout(t.Context(), 50*time.Millisecond)
		if err = calls[name](short); !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("%s while the store's memory is taken: %v", name, err)
		}
		cancel()
		taken.Release()
		if err = calls[name](t.Context()); err != nil {
			t.Fatalf("%s once the memory is free: %v", name, err)
		}
		if usage := s.runtime.Memory(); usage.Used != 0 {
			t.Fatalf("%s kept %d bytes", name, usage.Used)
		}
	}
	reader, _, err := media.Open(t.Context(), "kept")
	if err != nil {
		t.Fatal(err)
	}
	if usage := s.runtime.Memory(); usage.Used != 100 {
		t.Fatalf("an open inline reader holds %d bytes of the store's memory", usage.Used)
	}
	if err = reader.Close(); err != nil || s.runtime.Memory().Used != 0 {
		t.Fatalf("a closed reader holds %d bytes: %v", s.runtime.Memory().Used, err)
	}
}

// an upload left without calls once its context ended is aborted by
// maintenance: its file, slot and memory given back
func TestMaintenanceAbortsAnUploadItsContextLeft(t *testing.T) {
	s := openTestStoreWith(t, t.TempDir(), tinystore.Options{Memory: inlineSize}, Options{})
	media := openTestBucket(t, s, "media")
	ended, cancel := context.WithCancel(t.Context())
	upload, err := media.Create(ended, "left")
	if err != nil {
		t.Fatal(err)
	}
	if _, err = upload.Write(randomBytes(1, 1<<20)); err != nil {
		t.Fatal(err)
	}
	if done := s.maintain(t); done.Aborted != 0 {
		t.Fatalf("maintenance aborted %d uploads whose context lives", done.Aborted)
	}
	cancel()
	if done := s.maintain(t); done.Aborted != 1 {
		t.Fatalf("maintenance aborted %d uploads", done.Aborted)
	}
	s.mustHoldFiles(t, 0)
	if usage := s.runtime.Memory(); usage.Used != 0 {
		t.Fatalf("an aborted upload holds %d bytes of the store's memory", usage.Used)
	}
	if _, err = upload.Commit(t.Context()); !errors.Is(err, context.Canceled) {
		t.Fatalf("a Commit after maintenance aborted the upload: %v", err)
	}
}
