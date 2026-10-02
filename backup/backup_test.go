package backup

import (
	"archive/zip"
	"bytes"
	"context"
	"crypto/sha256"
	"errors"
	"hash"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"testing/fstest"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/blobs"
	"github.com/tinyshed/tinystore/metrics"
	"github.com/tinyshed/tinystore/records"
	"github.com/tinyshed/tinystore/sqldb"
)

var migrations = fstest.MapFS{"001_notes.sql": {Data: []byte(`create table notes (id integer primary key, title text not null) strict;`)}}

var epoch = time.Date(2026, 9, 24, 12, 0, 0, 0, time.UTC)

type engines struct {
	store *tinystore.Store
	cpu   *metrics.Store
	app   *sqldb.DB
	logs  *records.Store
	files *blobs.Bucket
}

func openEngines(t *testing.T, dir string) engines {
	t.Helper()
	store, err := tinystore.Open(t.Context(), dir, tinystore.Options{Manual: true, Clock: func() time.Time { return epoch }})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close(context.Background()) })
	cpu, err := metrics.Open(t.Context(), store, metrics.Options{})
	if err != nil {
		t.Fatal(err)
	}
	app, err := sqldb.Open(t.Context(), store, "app", migrations, nil)
	if err != nil {
		t.Fatal(err)
	}
	logs, err := records.Open(t.Context(), store, records.Options{})
	if err != nil {
		t.Fatal(err)
	}
	objects, err := blobs.Open(t.Context(), store, blobs.Options{})
	if err != nil {
		t.Fatal(err)
	}
	files, err := blobs.OpenBucket(t.Context(), objects, "files")
	if err != nil {
		t.Fatal(err)
	}
	return engines{store: store, cpu: cpu, app: app, logs: logs, files: files}
}

// the objects backupOf keeps: one inline, one in a file, and a copy sharing its bytes
var objects = map[string][]byte{
	"notes/1/list.txt":  []byte("milk, bread, eggs, tea"),
	"notes/1/photo.jpg": bytes.Repeat([]byte{0xd8, 0xff, 0xe0}, 300_000),
	"notes/2/photo.jpg": bytes.Repeat([]byte{0xd8, 0xff, 0xe0}, 300_000),
}

func backupOf(t *testing.T) []byte {
	t.Helper()
	source := openEngines(t, t.TempDir())
	source.cpu.Gauge("temperature").Set(21.5)
	if err := source.cpu.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if _, err := source.app.Exec(t.Context(), `insert into notes (title) values ('kept')`); err != nil {
		t.Fatal(err)
	}
	if err := source.logs.Handler("app", records.ConsoleOff).Handle(t.Context(), slog.NewRecord(epoch, slog.LevelInfo, "backed up", 0)); err != nil {
		t.Fatal(err)
	}
	if err := source.logs.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	for _, key := range []string{"notes/1/list.txt", "notes/1/photo.jpg"} {
		if _, err := source.files.Put(t.Context(), key, bytes.NewReader(objects[key])); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := source.files.Copy(t.Context(), "notes/1/photo.jpg", "notes/2/photo.jpg"); err != nil {
		t.Fatal(err)
	}

	var archive bytes.Buffer
	if err := Write(t.Context(), source.store, &archive); err != nil {
		t.Fatal(err)
	}
	return archive.Bytes()
}

func TestABackupRestoresEveryEngine(t *testing.T) {
	archive := backupOf(t)
	dir := filepath.Join(t.TempDir(), "restored")
	if err := Restore(t.Context(), dir, bytes.NewReader(archive), int64(len(archive))); err != nil {
		t.Fatal(err)
	}

	restored := openEngines(t, dir)
	results, err := restored.cpu.Read(t.Context(), metrics.Range{
		Name: "temperature", From: epoch.UnixMilli(), To: epoch.UnixMilli() + 1,
	})
	if err != nil || len(results) != 1 || results[0].Samples[0].Value != 21.5 {
		t.Fatalf("metrics: %+v, %v", results, err)
	}
	title, err := sqldb.Scalar[string](t.Context(), restored.app, `select title from notes`)
	if err != nil || title != "kept" {
		t.Fatalf("sql: %q, %v", title, err)
	}
	page, err := restored.logs.Scan(t.Context(), records.Query{From: epoch, To: epoch.Add(time.Second)})
	if err != nil || len(page.Records) != 1 || *page.Records[0].Body != "backed up" {
		t.Fatalf("records: %+v, %v", page.Records, err)
	}
	for key, data := range objects {
		mustHold(t, restored.files, key, data)
	}
}

func mustHold(t *testing.T, files *blobs.Bucket, key string, want []byte) {
	t.Helper()
	reader, found, err := files.Open(t.Context(), key)
	if err != nil || !found {
		t.Fatalf("blobs %s: %v, %v", key, found, err)
	}
	defer reader.Close()
	data, err := io.ReadAll(reader)
	if err != nil || !bytes.Equal(data, want) {
		t.Fatalf("blobs %s: %d bytes, %v", key, len(data), err)
	}
}

// a backup holds every object bit for bit: blobs.db deflated, and each file
// its copy names once, stored as it is, however many keys share it
func TestABackupRestoresEveryObject(t *testing.T) {
	archive := backupOf(t)
	reader, err := zip.NewReader(bytes.NewReader(archive), int64(len(archive)))
	if err != nil {
		t.Fatal(err)
	}
	files := 0
	for _, entry := range reader.File {
		stored := entry.Method == zip.Store
		if blob := strings.HasPrefix(entry.Name, "blobs/objects/"); blob != stored {
			t.Fatalf("%s: method %d", entry.Name, entry.Method)
		} else if blob {
			files++
		}
	}
	if files != 1 {
		t.Fatalf("the backup holds %d files of blobs, want the one two keys share", files)
	}
	changed := changedEntry(t, archive, blobEntry(t, reader))
	err = Restore(t.Context(), t.TempDir(), bytes.NewReader(changed), int64(len(changed)))
	if !errors.Is(err, tinystore.ErrCorrupt) {
		t.Fatalf("a changed file of blobs: %v", err)
	}
}

func blobEntry(t *testing.T, reader *zip.Reader) string {
	t.Helper()
	for _, entry := range reader.File {
		if strings.HasPrefix(entry.Name, "blobs/objects/") {
			return entry.Name
		}
	}
	t.Fatal("the backup holds no file of blobs")
	return ""
}

func TestAChangedByteIsRefusedAndLeavesNothing(t *testing.T) {
	archive := changedEntry(t, backupOf(t), "sql/app.db")
	dir := t.TempDir()
	err := Restore(t.Context(), dir, bytes.NewReader(archive), int64(len(archive)))
	if !errors.Is(err, tinystore.ErrCorrupt) {
		t.Fatalf("a changed file: %v", err)
	}
	if entries, _ := os.ReadDir(dir); len(entries) != 0 {
		t.Fatalf("a failed restore left %d entries", len(entries))
	}
}

func TestRestoreRefusesADirectoryInUse(t *testing.T) {
	archive := backupOf(t)
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "metrics.db"), []byte("live"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := Restore(t.Context(), dir, bytes.NewReader(archive), int64(len(archive))); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("restore over a directory with data: %v", err)
	}
}

func TestAClosedStoreTakesNoSnapshot(t *testing.T) {
	source := openEngines(t, t.TempDir())
	if err := source.store.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := Write(t.Context(), source.store, io.Discard); !errors.Is(err, tinystore.ErrClosed) {
		t.Fatalf("backup of a closed store: %v", err)
	}
}

// changedEntry rewrites one entry of the zip with its last byte flipped
func changedEntry(t *testing.T, archive []byte, name string) []byte {
	t.Helper()
	reader, err := zip.NewReader(bytes.NewReader(archive), int64(len(archive)))
	if err != nil {
		t.Fatal(err)
	}
	var out bytes.Buffer
	writer := zip.NewWriter(&out)
	for _, file := range reader.File {
		data := entryBytes(t, file)
		if file.Name == name {
			data[len(data)-1] ^= 1
		}
		target, createErr := writer.Create(file.Name)
		if createErr != nil {
			t.Fatal(createErr)
		}
		if _, err = target.Write(data); err != nil {
			t.Fatal(err)
		}
	}
	if err = writer.Close(); err != nil {
		t.Fatal(err)
	}
	return out.Bytes()
}

func entryBytes(t *testing.T, file *zip.File) []byte {
	t.Helper()
	source, err := file.Open()
	if err != nil {
		t.Fatal(err)
	}
	defer source.Close()
	data, err := io.ReadAll(source)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

// fill is a stream of size bytes of a repeated random chunk, summed as it
// is read, so that a large object takes no memory of the test's own
type fill struct {
	chunk []byte
	left  int64
	sum   hash.Hash
}

func (f *fill) Read(p []byte) (int, error) {
	if f.left == 0 {
		return 0, io.EOF
	}
	n := copy(p[:min(int64(len(p)), f.left)], f.chunk)
	f.left -= int64(n)
	f.sum.Write(p[:n])
	return n, nil
}

// An object past 4 GiB goes through a backup and back bit for bit, the zip's
// entry and directory in their zip64 forms. It writes 13 GiB, so it runs only
// when TINYSTORE_LARGE_BACKUP says.
func TestABackupRestoresAnObjectPast4GiB(t *testing.T) {
	if os.Getenv("TINYSTORE_LARGE_BACKUP") == "" {
		t.Skip("writes 13 GiB; set TINYSTORE_LARGE_BACKUP=1")
	}
	work := t.TempDir()
	source := openEngines(t, filepath.Join(work, "source"))
	chunk := make([]byte, 1<<20)
	for i := range chunk {
		chunk[i] = byte(i*7 + i/251)
	}
	film := &fill{chunk: chunk, left: 4<<30 + 12345, sum: sha256.New()}
	if _, err := source.files.Put(t.Context(), "film.mkv", film); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(work, "backup.zip")
	archive, err := os.Create(path)
	if err != nil {
		t.Fatal(err)
	}
	if err = Write(t.Context(), source.store, archive); err != nil {
		t.Fatal(err)
	}
	size, err := archive.Seek(0, io.SeekEnd)
	if err != nil {
		t.Fatal(err)
	}
	if err = Restore(t.Context(), filepath.Join(work, "restored"), archive, size); err != nil {
		t.Fatal(err)
	}
	_ = archive.Close()

	restored := openEngines(t, filepath.Join(work, "restored"))
	reader, found, err := restored.files.Open(t.Context(), "film.mkv")
	if err != nil || !found {
		t.Fatal(found, err)
	}
	defer reader.Close()
	sum := sha256.New()
	if _, err = io.Copy(sum, reader); err != nil || !bytes.Equal(sum.Sum(nil), film.sum.Sum(nil)) {
		t.Fatalf("the restored object differs: %v", err)
	}
}
