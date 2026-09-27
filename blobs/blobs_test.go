package blobs

import (
	"bytes"
	"context"
	"errors"
	"go/parser"
	"go/token"
	"io"
	"io/fs"
	"math/rand/v2"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// testClock is the store's clock, moved by the test
type testClock struct {
	mu  sync.Mutex
	now time.Time
}

func (c *testClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *testClock) advance(d time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.now = c.now.Add(d)
}

var testStart = time.Date(2026, 9, 27, 12, 0, 0, 0, time.UTC)

type testStore struct {
	*Store
	runtime *tinystore.Store
	clock   *testClock
	dir     string
}

// openTestStore opens a Manual store on dir with a clock the test moves, and
// blobs inside it; the test closes it
func openTestStore(t testing.TB, dir string) *testStore {
	t.Helper()
	return openTestStoreWith(t, dir, tinystore.Options{}, Options{})
}

func openTestStoreWith(t testing.TB, dir string, runtime tinystore.Options, options Options) *testStore {
	t.Helper()
	clock := &testClock{now: testStart}
	runtime.Manual, runtime.Clock = true, clock.Now
	store, err := tinystore.Open(t.Context(), dir, runtime)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close(context.Background()) })
	objects, err := Open(t.Context(), store, options)
	if err != nil {
		t.Fatal(err)
	}
	return &testStore{Store: objects, runtime: store, clock: clock, dir: dir}
}

func (s *testStore) reopen(t *testing.T) *testStore {
	t.Helper()
	if err := s.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	reopened := openTestStore(t, s.dir)
	reopened.clock.now = s.clock.Now()
	return reopened
}

func (s *testStore) maintain(t *testing.T) Maintenance {
	t.Helper()
	done, err := s.Maintain(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	return done
}

func openTestBucket(t testing.TB, s *testStore, name string, options ...BucketOption) *Bucket {
	t.Helper()
	bucket, err := OpenBucket(t.Context(), s.Store, name, options...)
	if err != nil {
		t.Fatal(err)
	}
	return bucket
}

// randomBytes are size bytes from a seeded source, as media is: nothing a
// codec could compress
func randomBytes(seed uint64, size int) []byte {
	random := rand.NewChaCha8([32]byte{byte(seed), byte(seed >> 8), byte(seed >> 16)})
	data := make([]byte, size)
	_, _ = random.Read(data)
	return data
}

// sizes an object may have: empty, inline, the inline bound, and files
var testSizes = []int{0, 1, 100, inlineSize - 1, inlineSize, inlineSize + 1, 1 << 20}

func mustPut(t testing.TB, bucket *Bucket, key string, data []byte, options ...Option) Object {
	t.Helper()
	object, err := bucket.Put(t.Context(), key, bytes.NewReader(data), options...)
	if err != nil {
		t.Fatalf("Put %s: %v", key, err)
	}
	return object
}

// readAll opens key and reads it whole, failing the test on an error
func readAll(t testing.TB, bucket *Bucket, key string) ([]byte, Object, bool) {
	t.Helper()
	reader, found, err := bucket.Open(t.Context(), key)
	if err != nil {
		t.Fatalf("Open %s: %v", key, err)
	}
	if !found {
		return nil, Object{}, false
	}
	defer reader.Close()
	data, err := io.ReadAll(reader)
	if err != nil {
		t.Fatalf("read %s: %v", key, err)
	}
	return data, reader.Object, true
}

func mustHold(t testing.TB, bucket *Bucket, key string, want []byte) {
	t.Helper()
	data, _, found := readAll(t, bucket, key)
	if !found || !bytes.Equal(data, want) {
		t.Fatalf("%s holds %d bytes, found %v; want %d", key, len(data), found, len(want))
	}
}

func mustBeAbsent(t testing.TB, bucket *Bucket, key string) {
	t.Helper()
	if _, _, found := readAll(t, bucket, key); found {
		t.Fatalf("%s is still there", key)
	}
	if _, found, err := bucket.Stat(t.Context(), key); err != nil || found {
		t.Fatalf("Stat %s: found %v, %v", key, found, err)
	}
}

// filesIn is the names of the files under a directory of blobs/
func (s *testStore) filesIn(t testing.TB, dir string) []string {
	t.Helper()
	var names []string
	err := filepath.WalkDir(filepath.Join(s.Store.dir, dir), func(path string, entry fs.DirEntry, err error) error {
		if err == nil && !entry.IsDir() {
			names = append(names, entry.Name())
		}
		if errors.Is(err, fs.ErrNotExist) {
			return nil
		}
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return names
}

// mustHoldFiles fails the test unless objects/ holds files files and uploads/ none
func (s *testStore) mustHoldFiles(t testing.TB, files int) {
	t.Helper()
	if held := s.filesIn(t, objectsDir); len(held) != files {
		t.Fatalf("objects/ holds %d files, %v; want %d", len(held), held, files)
	}
	if left := s.filesIn(t, uploadsDir); len(left) != 0 {
		t.Fatalf("uploads/ holds %v", left)
	}
}

func TestPutThenOpenEverySize(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	for i, size := range testSizes {
		key := "sizes/" + strconv.Itoa(size)
		data := randomBytes(uint64(i), size)
		object := mustPut(t, media, key, data, ContentType("application/octet-stream"), Meta("n", strconv.Itoa(i)))
		if object.Key != key || object.Size != int64(size) || object.ContentType != "application/octet-stream" ||
			object.Meta["n"] != strconv.Itoa(i) || !object.Modified.Equal(testStart) || !object.Expires.IsZero() {
			t.Fatalf("Put of %d bytes returned %+v", size, object)
		}
		got, opened, found := readAll(t, media, key)
		if !found || !bytes.Equal(got, data) || opened.ETag != object.ETag || opened.Meta["n"] != strconv.Itoa(i) {
			t.Fatalf("Open of %d bytes: %d bytes, %+v", size, len(got), opened)
		}
		stat, found, err := media.Stat(t.Context(), key)
		if err != nil || !found || stat.ETag != object.ETag || stat.Size != int64(size) {
			t.Fatalf("Stat of %d bytes: %+v, %v, %v", size, stat, found, err)
		}
	}
	s.mustHoldFiles(t, 2) // inlineSize+1 and 1 MiB
}

// an ETag is the bytes', quoted as a header carries it: equal bytes, one ETag
func TestAnETagIsTheBytes(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	for _, size := range []int{10, 1 << 20} {
		data := randomBytes(7, size)
		first := mustPut(t, media, "a", data)
		second := mustPut(t, media, "b", data, ContentType("other"))
		third := mustPut(t, media, "c", randomBytes(8, size))
		if first.ETag != second.ETag || first.ETag == third.ETag || !strings.HasPrefix(first.ETag, `"`) ||
			len(first.ETag) != 2+2*etagBytes {
			t.Fatalf("ETags of %d bytes: %s %s %s", size, first.ETag, second.ETag, third.ETag)
		}
	}
}

// an upload's bytes arrive by Write in pieces of any size, and the object is
// what they were, in order
func TestAnUploadTakesItsBytesInPieces(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	for i, size := range testSizes {
		data := randomBytes(uint64(i), size)
		upload, err := media.Create(t.Context(), "pieces", ContentType("video/mp4"))
		if err != nil {
			t.Fatal(err)
		}
		for rest := data; len(rest) > 0; {
			n := min(len(rest), 1+rand.IntN(5000))
			if _, err = upload.Write(rest[:n]); err != nil {
				t.Fatal(err)
			}
			rest = rest[n:]
		}
		object, err := upload.Commit(t.Context())
		if err != nil || object.Size != int64(size) {
			t.Fatalf("Commit of %d bytes: %+v, %v", size, object, err)
		}
		upload.Abort()
		mustHold(t, media, "pieces", data)
	}
	s.mustHoldFiles(t, 1)
}

// a Put of a stream that says nothing of its length gathers what may stay
// inline, then spills to a file
func TestAPutOfAStreamOfUnknownLength(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	for i, size := range testSizes {
		data := randomBytes(uint64(i), size)
		stream := io.MultiReader(bytes.NewReader(data[:size/2]), bytes.NewReader(data[size/2:]))
		object, err := media.Put(t.Context(), "stream", stream)
		if err != nil || object.Size != int64(size) {
			t.Fatalf("Put of %d bytes: %+v, %v", size, object, err)
		}
		mustHold(t, media, "stream", data)
	}
	s.mustHoldFiles(t, 1)
}

// the engine's packages import no net/http: an application serves an object
// with the standard library's http.ServeContent, and links the server itself
func TestBlobsImportsNoHTTP(t *testing.T) {
	names, err := filepath.Glob("*.go")
	if err != nil {
		t.Fatal(err)
	}
	for _, name := range names {
		if strings.HasSuffix(name, "_test.go") {
			continue
		}
		parsed, err := parser.ParseFile(token.NewFileSet(), name, nil, parser.ImportsOnly)
		if err != nil {
			t.Fatal(err)
		}
		for _, imported := range parsed.Imports {
			if strings.HasPrefix(imported.Path.Value, `"net`) {
				t.Errorf("%s imports %s", name, imported.Path.Value)
			}
		}
	}
}

// reopening the store keeps every object and removes nothing it should keep
func TestObjectsSurviveAReopen(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	stored := map[string][]byte{}
	for i, size := range testSizes {
		key := "kept/" + strconv.Itoa(i)
		stored[key] = randomBytes(uint64(i), size)
		mustPut(t, media, key, stored[key])
	}
	s = s.reopen(t)
	media = openTestBucket(t, s, "media")
	for key, data := range stored {
		mustHold(t, media, key, data)
	}
	s.mustHoldFiles(t, 2)
}
