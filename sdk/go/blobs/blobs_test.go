package blobs_test

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/blobs"
	"tinyshed.org/tinystore/internal/ways"
)

func pattern(n int, seed byte) []byte {
	out := make([]byte, n)
	for i := range out {
		out[i] = byte(i*31) + seed
	}
	return out
}

func is(t *testing.T, err error, kind error) {
	t.Helper()
	if !errors.Is(err, kind) {
		t.Fatalf("got %v, want %v", err, kind)
	}
}

func readAll(t *testing.T, file *blobs.StoredFile) []byte {
	t.Helper()
	data, err := io.ReadAll(file)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func TestAFileComesBackByteForByteEmptyInlineInOneCallAndInPieces(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		sizes := ways.Must(blobs.OpenFiles(store, "sizes"))
		for _, size := range []int{0, 1, 16 << 10, 70_000, 3<<20 + 17} {
			data := pattern(size, 7)
			put := ways.Must(sizes.Put(ctx, fmt.Sprintf("f%d", size), bytes.NewReader(data)))
			sum := sha256.Sum256(data)
			if put.Size != int64(size) || put.ETag != `"`+hex.EncodeToString(sum[:])+`"` || put.ContentType != "application/octet-stream" {
				t.Fatalf("put %+v", put)
			}
			head, found := ways.Must2(sizes.Head(ctx, fmt.Sprintf("f%d", size)))
			if !found || head.ETag != put.ETag {
				t.Fatalf("head %+v", head)
			}
			file, found := ways.Must2(sizes.Get(ctx, fmt.Sprintf("f%d", size)))
			if !found || !bytes.Equal(readAll(t, file), data) {
				t.Fatalf("the file of %d bytes comes back whole", size)
			}
			_ = file.Close()
		}
		if _, found := ways.Must2(sizes.Get(ctx, "nothing")); found {
			t.Fatal("no file is none")
		}
	})
}

// slow is a reader that knows no length and gives its bytes a chunk at a time.
type slow struct{ chunks [][]byte }

func (s *slow) Read(p []byte) (int, error) {
	if len(s.chunks) == 0 {
		return 0, io.EOF
	}
	n := copy(p, s.chunks[0])
	s.chunks[0] = s.chunks[0][n:]
	if len(s.chunks[0]) == 0 {
		s.chunks = s.chunks[1:]
	}
	return n, nil
}

func TestAReaderOfUnknownLengthGoesInPiecesWithItsMeta(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		bodies := ways.Must(blobs.OpenFiles(store, "bodies"))
		chunks := [][]byte{pattern(100_000, 1), pattern(3, 2), pattern(3<<20, 11)}
		whole := bytes.Join(chunks, nil)
		put := ways.Must(bodies.Put(ctx, "iterated.bin", &slow{chunks}, blobs.ContentType("application/x-test"), blobs.Meta(map[string]string{"name": "It.bin"})))
		if put.Size != int64(len(whole)) {
			t.Fatalf("put %+v", put)
		}
		file, _ := ways.Must2(bodies.Get(ctx, "iterated.bin"))
		if file.ContentType != "application/x-test" || file.Meta["name"] != "It.bin" || !bytes.Equal(readAll(t, file), whole) {
			t.Fatalf("file %+v", file.FileInfo)
		}
	})
}

func TestALargeFileReadAfterAReplaceIsAConflictAndSeeksAsServeContentAsks(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		kept := ways.Must(blobs.OpenFiles(store, "kept-bytes"))
		ways.Must(kept.Put(ctx, "small.txt", strings.NewReader("first")))
		small, _ := ways.Must2(kept.Get(ctx, "small.txt"))
		ways.Must(kept.Put(ctx, "small.txt", strings.NewReader("second")))
		if string(readAll(t, small)) != "first" {
			t.Fatal("a small file keeps the bytes its get brought")
		}
		film := pattern(5<<20, 13)
		ways.Must(kept.Put(ctx, "film.mp4", bytes.NewReader(film), blobs.ContentType("video/mp4")))
		file, _ := ways.Must2(kept.Get(ctx, "film.mp4"))
		part := make([]byte, 3_000_000)
		n, err := file.ReadAt(part, 1_000_000)
		if err != nil || n != len(part) || !bytes.Equal(part, film[1_000_000:4_000_000]) {
			t.Fatalf("read at %d %v", n, err)
		}
		ways.Must(file.Seek(-500, io.SeekEnd))
		if tail := readAll(t, file); !bytes.Equal(tail, film[len(film)-500:]) {
			t.Fatalf("the last 500 bytes, %d", len(tail))
		}
		recorder := httptest.NewRecorder()
		request := httptest.NewRequest(http.MethodGet, "/film.mp4", nil)
		request.Header.Set("Range", "bytes=100-199")
		ways.Must(file.Seek(0, io.SeekStart))
		http.ServeContent(recorder, request, "", file.LastModified, file)
		if recorder.Code != http.StatusPartialContent || !bytes.Equal(recorder.Body.Bytes(), film[100:200]) {
			t.Fatalf("served %d, %d bytes", recorder.Code, recorder.Body.Len())
		}
		_ = file.Close()
		file, _ = ways.Must2(kept.Get(ctx, "film.mp4"))
		ways.Must(kept.Put(ctx, "film.mp4", bytes.NewReader(pattern(5, 1))))
		_, err = io.ReadAll(file)
		is(t, err, tinystore.ErrConflict)
	})
}

func TestAFolderIsWholeSegmentsListedCountedAndCleared(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		media := ways.Must(blobs.OpenFiles(store, "media"))
		ways.Must(media.Folder("users", 42).Put(ctx, "photos/1.jpg", strings.NewReader("one")))
		ways.Must(media.Put(ctx, "users/42/photos/10.jpg", strings.NewReader("ten")))
		ways.Must(media.Put(ctx, "users/420/photos/1.jpg", strings.NewReader("other")))
		user := media.Folder("users", 42)
		page := ways.Must(user.List(ctx, blobs.Prefix("photos/1")))
		if len(page.Files) != 2 || page.Files[0].Path != "photos/1.jpg" || page.Next != "" {
			t.Fatalf("page %+v", page)
		}
		first := ways.Must(user.List(ctx, blobs.Limit(1)))
		rest := ways.Must(user.List(ctx, blobs.After(first.Next)))
		if len(rest.Files) != 1 || rest.Files[0].Path != "photos/10.jpg" {
			t.Fatalf("rest %+v", rest)
		}
		var all []string
		for file, err := range media.All(ctx) {
			if err != nil {
				t.Fatal(err)
			}
			all = append(all, file.Path)
		}
		if strings.Join(all, ",") != "users/42/photos/1.jpg,users/42/photos/10.jpg,users/420/photos/1.jpg" {
			t.Fatalf("all %v", all)
		}
		if usage := ways.Must(user.Usage(ctx)); usage != (blobs.Usage{Count: 2, Size: 6}) {
			t.Fatalf("usage %+v", usage)
		}
		ways.Must(0, user.Clear(ctx))
		if usage := ways.Must(user.Usage(ctx)); usage != (blobs.Usage{}) {
			t.Fatalf("usage %+v", usage)
		}
		_, err := media.Folder("users/42").Put(ctx, "a", strings.NewReader("x"))
		is(t, err, tinystore.ErrInvalid)
		_, err = media.Put(ctx, "../a", strings.NewReader("x"))
		is(t, err, tinystore.ErrInvalid)
	})
}

func TestIfMatchCreateCopyAndRenameWriteOnlyAsTheySay(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		docs := ways.Must(blobs.OpenFiles(store, "conditions"))
		first := ways.Must(docs.Put(ctx, "notes.md", strings.NewReader("v1")))
		ways.Must(docs.Put(ctx, "notes.md", strings.NewReader("v2"), blobs.IfMatch(first.ETag)))
		_, err := docs.Put(ctx, "notes.md", strings.NewReader("v3"), blobs.IfMatch(first.ETag))
		is(t, err, tinystore.ErrConflict)
		if !ways.Must(docs.Create(ctx, "new.md", strings.NewReader("a"))) || ways.Must(docs.Create(ctx, "new.md", strings.NewReader("b"))) {
			t.Fatal("create says whether it wrote")
		}
		source := ways.Must(docs.Put(ctx, "a.md", strings.NewReader("alpha")))
		copied := ways.Must(docs.Copy(ctx, "a.md", "b.md"))
		if copied.Path != "b.md" || copied.ETag != source.ETag {
			t.Fatalf("copy %+v", copied)
		}
		_, err = docs.Copy(ctx, "a.md", "b.md")
		is(t, err, tinystore.ErrConflict)
		target := ways.Must(docs.Put(ctx, "c.md", strings.NewReader("gamma")))
		if renamed := ways.Must(docs.Rename(ctx, "a.md", "c.md", blobs.IfMatch(target.ETag))); renamed.ETag != source.ETag {
			t.Fatalf("renamed %+v", renamed)
		}
		_, err = docs.Rename(ctx, "a.md", "d.md")
		is(t, err, tinystore.ErrNotFound)
		ways.Must(0, docs.Delete(ctx, "b.md"))
		ways.Must(0, docs.Delete(ctx, "b.md"))
	})
}

func TestAFileExpiresByItsOwnTermOrItsFiles(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		about := func(at time.Time, term time.Duration) bool {
			return !at.IsZero() && (time.Until(at)-term).Abs() < time.Minute
		}
		reports := ways.Must(blobs.OpenFiles(store, "reports", blobs.TTL(24*time.Hour)))
		if !about(ways.Must(reports.Put(ctx, "a.csv", strings.NewReader("a"))).Expires, 24*time.Hour) {
			t.Fatal("the files' term")
		}
		if !about(ways.Must(reports.Put(ctx, "b.csv", strings.NewReader("b"), blobs.TTL(7*24*time.Hour))).Expires, 7*24*time.Hour) {
			t.Fatal("a file's own term")
		}
		ways.Must(0, reports.Expire(ctx, "a.csv", time.Hour))
		if head, _ := ways.Must2(reports.Head(ctx, "a.csv")); !about(head.Expires, time.Hour) {
			t.Fatalf("expires %v", head.Expires)
		}
		is(t, reports.Expire(ctx, "none.csv", time.Hour), tinystore.ErrNotFound)
	})
}

func TestAnUploadAppearsAtItsCommitAndOneAbortedLeavesNothing(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		uploads := ways.Must(blobs.OpenFiles(store, "uploads"))
		line := strings.Repeat("x", 100_000) + "\n"
		upload := ways.Must(uploads.Upload(ctx, "2026-10/report.csv", blobs.ContentType("text/csv")))
		for range 40 {
			ways.Must(io.WriteString(upload, line))
		}
		if _, found := ways.Must2(uploads.Head(ctx, "2026-10/report.csv")); found {
			t.Fatal("an upload is nowhere before its commit")
		}
		info := ways.Must(upload.Commit(ctx))
		if info.Size != int64(40*len(line)) || info.ContentType != "text/csv" {
			t.Fatalf("info %+v", info)
		}
		_ = upload.Abort()
		left := ways.Must(uploads.Upload(ctx, "left.bin"))
		ways.Must(left.Write(pattern(3<<20, 1)))
		_ = left.Abort()
		if _, found := ways.Must2(uploads.Head(ctx, "left.bin")); found {
			t.Fatal("an aborted upload leaves nothing")
		}
		small := ways.Must(blobs.OpenFiles(store, "small", blobs.MaxFileSize(64<<10)))
		_, err := small.Put(ctx, "big", bytes.NewReader(pattern(65_537, 1)))
		is(t, err, tinystore.ErrLimit)
		_, err = small.Put(ctx, "big", &slow{[][]byte{pattern(40_000, 1), pattern(40_000, 2)}})
		is(t, err, tinystore.ErrLimit)
		if _, found := ways.Must2(small.Head(ctx, "big")); found {
			t.Fatal("a refused file leaves nothing")
		}
	})
}

func TestAWholeReadOfAChangedByteFailsCorruptBeforeItsEnd(t *testing.T) {
	ways.EachWay(t, func(t *testing.T, way ways.Way) {
		ctx := context.Background()
		dir := t.TempDir()
		store := way.Open(t, dir)
		t.Cleanup(func() { _ = store.Close() })
		checked := ways.Must(blobs.OpenFiles(store, "checked"))
		data := pattern(2<<20+5, 3)
		ways.Must(checked.Put(ctx, "a.bin", bytes.NewReader(data)))
		var stored []string
		_ = filepath.WalkDir(filepath.Join(way.Home(dir), "blobs", "objects"), func(path string, entry fs.DirEntry, err error) error {
			if info, _ := entry.Info(); err == nil && info.Mode().IsRegular() && info.Size() == int64(len(data)) {
				stored = append(stored, path)
			}
			return nil
		})
		if len(stored) != 1 {
			t.Fatalf("the file's bytes are in %v", stored)
		}
		changed := ways.Must(os.ReadFile(stored[0]))
		changed[4096] ^= 0x5a
		ways.Must(0, os.WriteFile(stored[0], changed, 0o600))
		file, _ := ways.Must2(checked.Get(ctx, "a.bin"))
		read, err := io.ReadAll(file)
		is(t, err, tinystore.ErrCorrupt)
		if len(read) >= len(data) {
			t.Fatalf("a corrupt read gave %d bytes of %d", len(read), len(data))
		}
		_ = file.Close()
		again, _ := ways.Must2(checked.Get(ctx, "a.bin"))
		part := make([]byte, 8192)
		if n, err := again.ReadAt(part, 0); n != len(part) || err != nil {
			t.Fatalf("a range is not checked: %d %v", n, err)
		}
	})
}

func TestAReadClosedMidWayLeavesTheConnectionToTheNextCall(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		films := ways.Must(blobs.OpenFiles(store, "films"))
		film := pattern(8<<20, 13)
		ways.Must(films.Put(ctx, "film.mp4", bytes.NewReader(film)))
		for i := range 20 {
			file, _ := ways.Must2(films.Get(ctx, "film.mp4"))
			ways.Must(file.Seek(int64(i)*1000, io.SeekStart))
			part := make([]byte, 100)
			ways.Must(io.ReadFull(file, part))
			if !bytes.Equal(part, film[i*1000:i*1000+100]) {
				t.Fatalf("read %d", i)
			}
			_ = file.Close()
			ways.Must(films.Put(ctx, fmt.Sprintf("n%d", i), strings.NewReader("abc")))
		}
	})
}
