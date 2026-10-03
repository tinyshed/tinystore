package main

import (
	"bytes"
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/records"
)

// backup writes a zip of a store its server serves, and restore takes it back
// into an empty directory, where the store opens with what was kept
func TestBackupWritesAZipThatRestoreTakesBack(t *testing.T) {
	dir, conn := servedStore(t)
	appendLines(t, conn, "api", "info", "kept across the backup")
	zipped := filepath.Join(t.TempDir(), "backup.zip")

	var out, stderr bytes.Buffer
	if err := backupStore(t.Context(), []string{dir, zipped}, &out, &stderr); err != nil {
		t.Fatal(err, stderr.String())
	}
	if !strings.Contains(out.String(), "backed up to "+zipped) {
		t.Fatalf("backup said %q", out.String())
	}
	if parts, _ := filepath.Glob(zipped + ".*.part"); len(parts) != 0 {
		t.Fatalf("a backup left %v", parts)
	}

	restored := filepath.Join(t.TempDir(), "restored")
	out.Reset()
	if err := restoreStore(t.Context(), []string{zipped, restored}, &out, &stderr); err != nil {
		t.Fatal(err, stderr.String())
	}
	store, err := tinystore.Open(t.Context(), restored, tinystore.Options{Manual: true})
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close(context.Background())
	logs, err := records.Open(t.Context(), store, records.Options{})
	if err != nil {
		t.Fatal(err)
	}
	page, err := logs.Scan(t.Context(), records.Query{Since: time.Hour})
	if err != nil || len(page.Records) != 1 || page.Records[0].Body == nil ||
		*page.Records[0].Body != "kept across the backup" {
		t.Fatalf("the restored store holds %+v: %v", page.Records, err)
	}

	if err = restoreStore(t.Context(), []string{zipped, restored}, &out, &stderr); err == nil {
		t.Fatal("a restore into a directory that is not empty")
	}
	if err = backupStore(t.Context(), []string{dir}, &out, &stderr); err == nil {
		t.Fatal("a backup without its file")
	}
	if _, err = os.Stat(filepath.Join(dir, "server", "SERVE")); err != nil {
		t.Fatalf("the server serving the store left during its backup: %v", err)
	}
}
