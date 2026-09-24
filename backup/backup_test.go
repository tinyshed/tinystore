package backup

import (
	"archive/zip"
	"bytes"
	"context"
	"errors"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"testing"
	"testing/fstest"
	"time"

	"github.com/tinyshed/tinystore"
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
	app, err := sqldb.Open(t.Context(), store, "app", migrations)
	if err != nil {
		t.Fatal(err)
	}
	logs, err := records.Open(t.Context(), store, records.Options{})
	if err != nil {
		t.Fatal(err)
	}
	return engines{store: store, cpu: cpu, app: app, logs: logs}
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
	if err := source.logs.Handler().Handle(t.Context(), slog.NewRecord(epoch, slog.LevelInfo, "backed up", 0)); err != nil {
		t.Fatal(err)
	}
	if err := source.logs.Flush(t.Context()); err != nil {
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
		Matchers: []metrics.Label{{Name: "__name__", Value: "temperature"}},
		From:     epoch.UnixMilli(), To: epoch.UnixMilli() + 1,
	})
	if err != nil || len(results) != 1 || results[0].Samples[0].Value != 21.5 {
		t.Fatalf("metrics: %+v, %v", results, err)
	}
	title, err := sqldb.Scalar[string](t.Context(), restored.app, `select title from notes`)
	if err != nil || title != "kept" {
		t.Fatalf("sql: %q, %v", title, err)
	}
	lines, err := restored.logs.Read(t.Context(), records.Query{From: epoch, To: epoch.Add(time.Second)})
	if err != nil || len(lines) != 1 || lines[0].Message != "backed up" {
		t.Fatalf("records: %+v, %v", lines, err)
	}
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
