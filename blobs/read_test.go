package blobs

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"maps"
	"mime"
	"mime/multipart"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// a reader keeps what it opened while its key is deleted, replaced, expired
// or cleared, the file removed under it, on Windows too
func TestAReaderKeepsWhatItOpened(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	changes := map[string]func(){
		"delete": func() {
			if err := media.Delete(t.Context(), "film"); err != nil {
				t.Fatal(err)
			}
		},
		"replace": func() { mustPut(t, media, "film", randomBytes(2, 1<<20)) },
		"expiry": func() {
			s.clock.advance(2 * time.Hour)
			s.maintain(t)
		},
		"clear": func() {
			if err := media.Clear(t.Context()); err != nil {
				t.Fatal(err)
			}
		},
	}
	for name, change := range changes {
		t.Run(name, func(t *testing.T) {
			data := randomBytes(1, 1<<20)
			mustPut(t, media, "film", data, TTL(time.Hour))
			reader, found, err := media.Open(t.Context(), "film")
			if err != nil || !found {
				t.Fatal(found, err)
			}
			defer reader.Close()
			half := make([]byte, len(data)/2)
			if _, err = io.ReadFull(reader, half); err != nil {
				t.Fatal(err)
			}

			change()
			if files := len(s.filesIn(t, objectsDir)); name != "replace" && files != 0 {
				t.Fatalf("after the %s objects/ holds %d files", name, files)
			}
			rest, err := io.ReadAll(reader)
			if err != nil || !bytes.Equal(append(half, rest...), data) {
				t.Fatalf("the reader read %d bytes after the %s: %v", len(half)+len(rest), name, err)
			}
			if err = media.Delete(t.Context(), "film"); err != nil {
				t.Fatal(err)
			}
		})
	}
}

// an Open racing the replacements of its key opens the object its key names,
// looking the key up again when the file it found went with its old version
func TestAnOpenRacingAReplaceOpensTheNewObject(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	versions := map[string][]byte{}
	for i := range 3 {
		data := randomBytes(uint64(i), inlineSize+1+i)
		versions[mustPut(t, media, "film", data).ETag] = data
	}
	var readers sync.WaitGroup
	stop := make(chan struct{})
	for range 8 {
		readers.Go(func() { readWholeVersions(t, media, "film", versions, stop) })
	}
	for round := 0; round < 3000 && s.lookedAgain.Load() == 0; round++ {
		for _, data := range versions {
			if _, err := media.Put(t.Context(), "film", bytes.NewReader(data)); err != nil {
				t.Fatal(err)
			}
		}
	}
	close(stop)
	readers.Wait()
	if s.lookedAgain.Load() == 0 {
		t.Skip("no Open met a file removed under it in 3,000 rounds")
	}
}

// a file missing while its key still names it is ErrCorrupt naming the key,
// and so is an inline body that no longer hashes to its content
func TestAMissingFileIsCorruptNamingItsKey(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	mustPut(t, media, "users/42/film", randomBytes(1, 1<<20))
	s.damageFiles(t, func(path string) error { return os.Remove(path) })
	_, _, err := media.Open(t.Context(), "users/42/film")
	var named *KeyError
	if !errors.Is(err, tinystore.ErrCorrupt) || !errors.As(err, &named) || named.Path != "users/42/film" {
		t.Fatalf("an Open of a file gone missing: %v", err)
	}

	mustPut(t, media, "users/42/icon", randomBytes(2, 100))
	err = s.file.UpdatePrepared(t.Context(), func(w sqlite.Writer) error {
		_, execErr := w.ExecContext(t.Context(), `update bodies set bytes = zeroblob(100)`)
		return execErr
	})
	if err != nil {
		t.Fatal(err)
	}
	if _, _, err = media.Of("users", 42).Open(t.Context(), "icon"); !errors.Is(err, tinystore.ErrCorrupt) {
		t.Fatalf("an Open of an inline object changed in the file: %v", err)
	}
}

// damageFiles changes every file of objects/
func (s *testStore) damageFiles(t testing.TB, damage func(path string) error) {
	t.Helper()
	err := filepath.WalkDir(filepath.Join(s.Store.dir, objectsDir), func(path string, entry os.DirEntry, err error) error {
		if err != nil || entry.IsDir() {
			return err
		}
		return damage(path)
	})
	if err != nil {
		t.Fatal(err)
	}
}

// flipLastByte changes a file's last byte
func flipLastByte(path string) error {
	data, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	data[len(data)-1] ^= 0xff
	return os.WriteFile(path, data, 0o600)
}

// a whole read of a file with a changed byte fails before its end: io.Copy
// returns ErrCorrupt, and the bytes it copied are fewer than the object's
func TestAWholeReadOfAChangedByteFailsBeforeItsEnd(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	data := randomBytes(1, 1<<20)
	mustPut(t, media, "film", data)
	s.damageFiles(t, flipLastByte)

	reader, found, err := media.Open(t.Context(), "film")
	if err != nil || !found {
		t.Fatal(found, err)
	}
	defer reader.Close()
	var copied bytes.Buffer
	n, err := io.Copy(&copied, reader)
	if !errors.Is(err, tinystore.ErrCorrupt) || n >= int64(len(data)) {
		t.Fatalf("a whole read of a changed file copied %d of %d bytes: %v", n, len(data), err)
	}
	if !bytes.Equal(copied.Bytes(), data[:n]) {
		t.Fatal("the bytes before the change were not the object's")
	}

	sniffed, _, _ := media.Open(t.Context(), "film")
	defer sniffed.Close()
	if _, err = sniffed.Read(make([]byte, 512)); err != nil {
		t.Fatal(err)
	}
	if _, err = sniffed.Seek(0, io.SeekStart); err != nil {
		t.Fatal(err)
	}
	if _, err = io.Copy(io.Discard, sniffed); !errors.Is(err, tinystore.ErrCorrupt) {
		t.Fatalf("a whole read after a sniff of its type: %v", err)
	}
}

// a range reads its own bytes, whatever the rest of the file holds: ReadAt,
// a section and a Read after a Seek are not checked
func TestARangeReadsItsBytesAndNoOthers(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	inline, file := randomBytes(1, 10_000), randomBytes(2, 3<<20)
	mustPut(t, media, "inline", inline)
	mustPut(t, media, "file", file)
	s.damageFiles(t, flipLastByte)

	for key, data := range map[string][]byte{"inline": inline, "file": file} {
		reader, _, err := media.Open(t.Context(), key)
		if err != nil {
			t.Fatal(err)
		}
		for _, at := range []int{0, 1, 4095, 4096, len(data) / 2, len(data) - 100} {
			got := make([]byte, 64)
			n, readErr := reader.ReadAt(got, int64(at))
			if readErr != nil || n != 64 || !bytes.Equal(got, data[at:at+64]) {
				t.Fatalf("%s: a range of 64 at %d read %d bytes: %v", key, at, n, readErr)
			}
			section, sectionErr := io.ReadAll(io.NewSectionReader(reader, int64(at), 64))
			if sectionErr != nil || !bytes.Equal(section, data[at:at+64]) {
				t.Fatalf("%s: a section of 64 at %d: %v", key, at, sectionErr)
			}
		}
		if n, endErr := reader.ReadAt(make([]byte, 200), int64(len(data)-100)); n != 100 || !errors.Is(endErr, io.EOF) {
			t.Fatalf("%s: a range past the end read %d bytes: %v", key, n, endErr)
		}
		if _, err = reader.Seek(-200, io.SeekEnd); err != nil {
			t.Fatal(err)
		}
		if tail, err := io.ReadAll(reader); err != nil || len(tail) != 200 {
			t.Fatalf("%s: a read after a seek, a range, read %d bytes: %v", key, len(tail), err)
		}
		_ = reader.Close()
	}
}

// http.ServeContent serves an object from its reader, its ETag and its
// Modified: ranges, several ranges, If-Range, If-None-Match,
// If-Modified-Since and HEAD
func TestServeContentAnswersRangesAndConditions(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	for _, size := range []int{5_000, 2 << 20} {
		data := randomBytes(uint64(size), size)
		object := mustPut(t, media, "film", data, ContentType("video/mp4"))
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			reader, found, err := media.Open(r.Context(), "film")
			if err != nil || !found {
				http.Error(w, fmt.Sprint(found, err), http.StatusInternalServerError)
				return
			}
			defer reader.Close()
			w.Header().Set("ETag", reader.ETag)
			w.Header().Set("Content-Type", reader.ContentType)
			http.ServeContent(w, r, "", reader.Modified, reader)
		}))
		serveCases(t, server.URL, object, data)
		server.Close()
	}
}

func serveCases(t *testing.T, url string, object Object, data []byte) {
	t.Helper()
	whole := serve(t, http.MethodGet, url, nil)
	if whole.status != http.StatusOK || !bytes.Equal(whole.body, data) || whole.header.Get("ETag") != object.ETag {
		t.Fatalf("GET: %d, %d bytes, %q", whole.status, len(whole.body), whole.header.Get("ETag"))
	}
	ranged := serve(t, http.MethodGet, url, http.Header{"Range": {"bytes=100-299"}})
	if ranged.status != http.StatusPartialContent || !bytes.Equal(ranged.body, data[100:300]) {
		t.Fatalf("a range: %d, %d bytes", ranged.status, len(ranged.body))
	}
	parts := serve(t, http.MethodGet, url, http.Header{"Range": {"bytes=0-9,1000-1009"}})
	if got := multipartBodies(t, parts); len(got) != 2 || !bytes.Equal(got[1], data[1000:1010]) {
		t.Fatalf("two ranges: %d, %d parts", parts.status, len(got))
	}
	modified := object.Modified.UTC().Format(http.TimeFormat)
	for header, want := range map[string]int{
		"If-None-Match":     http.StatusNotModified,
		"If-Modified-Since": http.StatusNotModified,
	} {
		value := object.ETag
		if header == "If-Modified-Since" {
			value = modified
		}
		if got := serve(t, http.MethodGet, url, http.Header{header: {value}}); got.status != want {
			t.Fatalf("%s: %d, want %d", header, got.status, want)
		}
	}
	stale := serve(t, http.MethodGet, url, http.Header{"Range": {"bytes=0-9"}, "If-Range": {`"stale"`}})
	fresh := serve(t, http.MethodGet, url, http.Header{"Range": {"bytes=0-9"}, "If-Range": {object.ETag}})
	if stale.status != http.StatusOK || fresh.status != http.StatusPartialContent {
		t.Fatalf("If-Range: stale %d, fresh %d", stale.status, fresh.status)
	}
	head := serve(t, http.MethodHead, url, nil)
	if head.status != http.StatusOK || head.header.Get("Content-Length") != fmt.Sprint(len(data)) || len(head.body) != 0 {
		t.Fatalf("HEAD: %d, %q", head.status, head.header.Get("Content-Length"))
	}
}

type served struct {
	status int
	header http.Header
	body   []byte
}

func serve(t *testing.T, method, url string, header http.Header) served {
	t.Helper()
	request, err := http.NewRequestWithContext(t.Context(), method, url, nil)
	if err != nil {
		t.Fatal(err)
	}
	maps.Copy(request.Header, header)
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	body, err := io.ReadAll(response.Body)
	if err != nil {
		t.Fatal(err)
	}
	return served{status: response.StatusCode, header: response.Header, body: body}
}

func multipartBodies(t *testing.T, response served) [][]byte {
	t.Helper()
	_, params, err := mime.ParseMediaType(response.header.Get("Content-Type"))
	if err != nil {
		t.Fatal(err)
	}
	reader := multipart.NewReader(bytes.NewReader(response.body), params["boundary"])
	var bodies [][]byte
	for {
		part, err := reader.NextPart()
		if errors.Is(err, io.EOF) {
			return bodies
		}
		if err != nil {
			t.Fatal(err)
		}
		body, err := io.ReadAll(part)
		if err != nil {
			t.Fatal(err)
		}
		bodies = append(bodies, body)
	}
}

// Stat and Open see the same object, and a key's folder is the handle's
func TestStatNamesTheKeyUnderItsHandle(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	object := mustPut(t, media.Of("users", 42), "photos/1.jpg", []byte("jpeg"), ContentType("image/jpeg"))
	if object.Key != "photos/1.jpg" {
		t.Fatalf("a Put under a folder returned the key %q", object.Key)
	}
	for handle, key := range map[*Bucket]string{
		media: "users/42/photos/1.jpg", media.Of("users"): "42/photos/1.jpg", media.Of("users", 42, "photos"): "1.jpg",
	} {
		stat, found, err := handle.Stat(t.Context(), key)
		if err != nil || !found || stat.Key != key || stat.ETag != object.ETag || stat.ContentType != "image/jpeg" {
			t.Fatalf("Stat of %s: %+v, %v, %v", key, stat, found, err)
		}
	}
	if !strings.HasPrefix(object.ETag, `"`) {
		t.Fatal(object.ETag)
	}
}
