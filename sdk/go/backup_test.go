package tinystore_test

import (
	"bytes"
	"context"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/blobs"
	"tinyshed.org/tinystore/internal/ways"
	"tinyshed.org/tinystore/kv"
)

// photo is past the 2 MiB a stream takes before its client grants more.
var photo = func() []byte {
	out := make([]byte, 3<<20)
	for at := range out {
		out[at] = byte(at % 251)
	}
	return out
}()

func filled(t *testing.T, way ways.Way, dir string) *tinystore.Store {
	t.Helper()
	ctx := context.Background()
	ways.Must(0, os.MkdirAll(dir, 0o700))
	store := way.Open(t, dir)
	ways.Must(0, ways.Must(kv.OpenBucket[string](store, "codes")).Set(ctx, "K7Q2", "kept across the backup"))
	ways.Must(ways.Must(blobs.OpenFiles(store, "photos")).Put(ctx, "cat.jpg", bytes.NewReader(photo)))
	return store
}

// restored restores a zip with tinystore restore, opens the store, runs check
// on it and its directory, and lets it go.
func restored(t *testing.T, way ways.Way, zip string, check func(store *tinystore.Store, home string)) {
	t.Helper()
	back := t.TempDir()
	out, err := exec.Command(ways.Binary(), "restore", zip, way.Home(back)).CombinedOutput()
	if err != nil {
		t.Fatalf("restore: %v: %s", err, out)
	}
	opened := way.Open(t, back)
	defer opened.Close()
	check(opened, way.Home(back))
}

func TestABackupIsOneZipOfEveryEngineWhichRestoreTakesBackIntoAnEmptyDirectory(t *testing.T) {
	if ways.Binary() == "" {
		t.Skip("tinystore is not built here")
	}
	ways.EachWay(t, func(t *testing.T, way ways.Way) {
		ctx := context.Background()
		dir := t.TempDir()
		store := filled(t, way, filepath.Join(dir, "store"))
		zip := filepath.Join(dir, "every.zip")
		backup := ways.Must(store.Backup(ctx, zip))
		ways.Must(0, store.Close())
		if backup.Files == 0 || backup.Bytes <= int64(len(photo)) || backup.KeyLeftOut {
			t.Fatalf("backup %+v", backup)
		}
		if parts, _ := filepath.Glob(filepath.Join(dir, "*.part")); len(parts) != 0 {
			t.Fatalf("parts left %v", parts)
		}
		restored(t, way, zip, func(back *tinystore.Store, _ string) {
			code, _ := ways.Must2(ways.Must(kv.OpenBucket[string](back, "codes")).Get(ctx, "K7Q2"))
			if code != "kept across the backup" {
				t.Fatalf("code %q", code)
			}
			cat, found := ways.Must2(ways.Must(blobs.OpenFiles(back, "photos")).Get(ctx, "cat.jpg"))
			if !found || !bytes.Equal(ways.Must(io.ReadAll(cat)), photo) {
				t.Fatal("the photo did not come back whole")
			}
			_ = cat.Close()
		})
	})
}

func TestABackupKeepsAFileOfTheProgramsOnlyWhenIncludeNamesIt(t *testing.T) {
	if ways.Binary() == "" {
		t.Skip("tinystore is not built here")
	}
	ways.EachWay(t, func(t *testing.T, way ways.Way) {
		ctx := context.Background()
		dir := t.TempDir()
		store := filled(t, way, filepath.Join(dir, "store"))
		ways.Must(0, os.WriteFile(filepath.Join(way.Home(filepath.Join(dir, "store")), "secret.key"), []byte("k3y"), 0o600))
		named, unnamed := filepath.Join(dir, "named.zip"), filepath.Join(dir, "unnamed.zip")
		ways.Must(store.Backup(ctx, named, tinystore.Include("secret.key")))
		ways.Must(store.Backup(ctx, unnamed))
		ways.Must(0, store.Close())
		restored(t, way, named, func(_ *tinystore.Store, home string) {
			if text, err := os.ReadFile(filepath.Join(home, "secret.key")); err != nil || string(text) != "k3y" {
				t.Fatalf("named %q %v", text, err)
			}
		})
		restored(t, way, unnamed, func(_ *tinystore.Store, home string) {
			if _, err := os.Stat(filepath.Join(home, "secret.key")); err == nil {
				t.Fatal("a file nobody named was kept")
			}
		})
	})
}

func TestABackupThatFailsLeavesNoZip(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		zips := t.TempDir()
		_, err := store.Backup(context.Background(), filepath.Join(zips, "failed.zip"), tinystore.Include("missing.txt"))
		if err == nil || !strings.Contains(err.Error(), "missing.txt") {
			t.Fatalf("backup %v", err)
		}
		if left, _ := os.ReadDir(zips); len(left) != 0 {
			t.Fatalf("left %v", left)
		}
	})
}
