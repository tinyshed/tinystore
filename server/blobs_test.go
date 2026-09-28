package server

import (
	"bytes"
	"crypto/rand"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/tinyshed/tinystore/server/internal/client"
	"github.com/tinyshed/tinystore/server/wire"
)

func openBlobs(t *testing.T, conn *client.Conn, bucket wire.BlobsBucket) uint64 {
	t.Helper()
	body, err := conn.Call(t.Context(), wire.BlobsOpen, bucket)
	if err != nil {
		t.Fatal(err)
	}
	var handle wire.Handle
	if err := handle.Decode(body); err != nil {
		t.Fatal(err)
	}
	return handle.Handle
}

// put uploads data in DATA of 64 KiB and returns the object committed
func put(t *testing.T, conn *client.Conn, ask wire.BlobsCall, data []byte) (wire.BlobsObject, error) {
	t.Helper()
	st, err := conn.Open(t.Context(), wire.BlobsPut, ask, false)
	if err != nil {
		t.Fatal(err)
	}
	for sent := 0; ; {
		n := min(len(data)-sent, 64<<10)
		if sendErr := st.Send(t.Context(), data[sent:sent+n], sent+n == len(data)); sendErr != nil {
			t.Fatal(sendErr)
		}
		sent += n
		if sent == len(data) {
			break
		}
	}
	body, err := st.Response(t.Context())
	if err != nil {
		return wire.BlobsObject{}, err
	}
	var object wire.BlobsObject
	return object, object.Decode(body)
}

// get downloads an object's bytes, or says it found none
func get(t *testing.T, conn *client.Conn, ask wire.BlobsCall) (wire.BlobsObject, []byte, error) {
	t.Helper()
	st, err := conn.Open(t.Context(), wire.BlobsGet, ask, true)
	if err != nil {
		t.Fatal(err)
	}
	body, err := st.Response(t.Context())
	if err != nil {
		return wire.BlobsObject{}, nil, err
	}
	var object wire.BlobsObject
	if err := object.Decode(body); err != nil || !object.Found {
		return object, nil, err
	}
	var read []byte
	for {
		chunk, last, err := st.Next(t.Context())
		if err != nil {
			return object, read, err
		}
		read = append(read, chunk...)
		if last {
			return object, read, nil
		}
	}
}

func randomBytes(t *testing.T, n int) []byte {
	t.Helper()
	data := make([]byte, n)
	if _, err := rand.Read(data); err != nil {
		t.Fatal(err)
	}
	return data
}

func TestBlobsOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	avatars := openBlobs(t, conn, wire.BlobsBucket{Name: "avatars", MaxSize: 1 << 20})
	jpeg := "image/jpeg"

	for _, size := range []int{0, 100, 300 << 10} {
		data := randomBytes(t, size)
		key := wire.BlobsCall{Handle: avatars, Owners: []string{"7"}, Key: "original", Size: -1}
		upload := key
		upload.ContentType, upload.Meta, upload.Size = &jpeg, map[string]string{"width": "640"}, int64(size)
		written, err := put(t, conn, upload, data)
		if err != nil {
			t.Fatalf("a put of %d bytes: %v", size, err)
		}
		if !written.Found || written.Size != int64(size) || written.ContentType != jpeg ||
			written.Meta["width"] != "640" || !strings.HasPrefix(written.ETag, `"`) {
			t.Fatalf("a put of %d bytes wrote %+v", size, written)
		}
		object, read, err := get(t, conn, key)
		if err != nil || !bytes.Equal(read, data) || object.ETag != written.ETag {
			t.Fatalf("a get of %d bytes: %d read, %+v, %v", size, len(read), object, err)
		}
	}

	data := randomBytes(t, 200<<10)
	file := wire.BlobsCall{Handle: avatars, Owners: []string{"7"}, Key: "big", Size: -1}
	if _, err := put(t, conn, file, data); err != nil {
		t.Fatal(err)
	}
	window := file
	window.Offset, window.Length = 70_000, 1000
	if _, read, err := get(t, conn, window); err != nil || !bytes.Equal(read, data[70_000:71_000]) {
		t.Fatalf("a range: %d bytes, %v", len(read), err)
	}

	copied := file
	copied.To = "copy"
	if object, err := blobsDo(t, conn, wire.BlobsCopy, copied); err != nil || object.Size != int64(len(data)) {
		t.Fatalf("a copy: %+v %v", object, err)
	}
	moved := wire.BlobsCall{Handle: avatars, Owners: []string{"7"}, Key: "copy", To: "moved", Size: -1}
	if _, err := blobsDo(t, conn, wire.BlobsMove, moved); err != nil {
		t.Fatal(err)
	}
	if gone, _, err := get(t, conn, wire.BlobsCall{Handle: avatars, Owners: []string{"7"}, Key: "copy", Size: -1}); err != nil ||
		gone.Found {
		t.Fatalf("a moved object's old key: %+v %v", gone, err)
	}

	body, err := conn.Call(t.Context(), wire.BlobsUsage, wire.BlobsCall{Handle: avatars, Owners: []string{"7"}, Size: -1})
	var total wire.BlobsTotal
	if err != nil || total.Decode(body) != nil || total.Objects != 3 {
		t.Fatalf("usage: %+v %v", total, err)
	}

	stale := wire.BlobsCall{Handle: avatars, Owners: []string{"7"}, Key: "big", IfMatch: `"00"`, Size: -1}
	if _, err := conn.Call(t.Context(), wire.BlobsDelete, stale); codeOfError(err) != wire.CodeConflict {
		t.Fatalf("a delete of another version: %v", err)
	}
	if _, err := conn.Call(t.Context(), wire.BlobsClear, wire.BlobsCall{
		Handle: avatars, Owners: []string{"7"},
		Size: -1,
	}); err != nil {
		t.Fatal(err)
	}
	if object, err := blobsDo(t, conn, wire.BlobsStat, file); err != nil || object.Found {
		t.Fatalf("a cleared object: %+v %v", object, err)
	}
}

func blobsDo(t *testing.T, conn *client.Conn, method wire.Method, ask wire.BlobsCall) (wire.BlobsObject, error) {
	t.Helper()
	body, err := conn.Call(t.Context(), method, ask)
	if err != nil {
		return wire.BlobsObject{}, err
	}
	var object wire.BlobsObject
	return object, object.Decode(body)
}

func TestABlobsScanPagesAFolder(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	files := openBlobs(t, conn, wire.BlobsBucket{Name: "files"})
	for _, key := range []string{"a.txt", "b/c.txt", "d.txt"} {
		if _, err := put(t, conn, wire.BlobsCall{Handle: files, Owners: []string{"u1"}, Key: key, Size: -1},
			[]byte(key)); err != nil {
			t.Fatal(err)
		}
	}
	st, err := conn.Open(t.Context(), wire.BlobsScan, wire.BlobsCall{
		Handle: files, Owners: []string{"u1"}, Limit: 2,
		Size: -1,
	}, true)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := st.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	var keys []string
	var page wire.BlobsPage
	for {
		body, last, err := st.Next(t.Context())
		if err != nil {
			t.Fatal(err)
		}
		if last {
			if err := page.Decode(body); err != nil {
				t.Fatal(err)
			}
			break
		}
		var object wire.BlobsObject
		if err := object.Decode(body); err != nil {
			t.Fatal(err)
		}
		keys = append(keys, object.Key)
	}
	if strings.Join(keys, ",") != "a.txt,b/c.txt" || !page.More || page.After != "b/c.txt" {
		t.Fatalf("a page: %v %+v", keys, page)
	}
}

// an upload the client cancels, or whose stream says more bytes than it
// carries, leaves no object
func TestAnUploadThatDoesNotCommitLeavesNothing(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	exports := openBlobs(t, conn, wire.BlobsBucket{Name: "exports"})
	key := wire.BlobsCall{Handle: exports, Key: "notes.zip", Size: -1}

	st, err := conn.Open(t.Context(), wire.BlobsPut, key, false)
	if err != nil {
		t.Fatal(err)
	}
	if err := st.Send(t.Context(), randomBytes(t, 64<<10), false); err != nil {
		t.Fatal(err)
	}
	if err := st.Cancel(); err != nil {
		t.Fatal(err)
	}
	if _, err := st.Response(t.Context()); codeOfError(err) != wire.CodeCancelled {
		t.Fatalf("a cancelled upload: %v", err)
	}

	short := key
	short.Size = 1000
	if _, err := put(t, conn, short, []byte("fewer")); codeOfError(err) != wire.CodeInvalid {
		t.Fatalf("an upload shorter than its size: %v", err)
	}
	if object, err := blobsDo(t, conn, wire.BlobsStat, key); err != nil || object.Found {
		t.Fatalf("what the uploads left: %+v %v", object, err)
	}
}

// a whole read of an object whose file changed ends with corrupt instead of
// its last bytes
func TestAWholeReadOfAChangedObjectEndsCorrupt(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	photos := openBlobs(t, conn, wire.BlobsBucket{Name: "photos"})
	key := wire.BlobsCall{Handle: photos, Key: "cat.jpg", Size: -1}
	if _, err := put(t, conn, key, randomBytes(t, 200<<10)); err != nil {
		t.Fatal(err)
	}
	changeTheOneFile(t, filepath.Join(ts.root, "blobs", "objects"))

	_, read, err := get(t, conn, key)
	if codeOfError(err) != wire.CodeCorrupt || len(read) >= 200<<10 {
		t.Fatalf("a changed object read %d bytes: %v", len(read), err)
	}
}

// changeTheOneFile flips a byte in the one file under dir
func changeTheOneFile(t *testing.T, dir string) {
	t.Helper()
	var found []string
	if err := filepath.WalkDir(dir, func(path string, entry fs.DirEntry, err error) error {
		if err == nil && !entry.IsDir() {
			found = append(found, path)
		}
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if len(found) != 1 {
		t.Fatalf("%d files under %s", len(found), dir)
	}
	data, err := os.ReadFile(found[0])
	if err != nil {
		t.Fatal(err)
	}
	data[len(data)/2] ^= 0xff
	if err := os.WriteFile(found[0], data, 0o600); err != nil {
		t.Fatal(err)
	}
}
