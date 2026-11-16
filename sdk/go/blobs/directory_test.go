package blobs_test

// Directories as docs/guides/blobs/directories.md has them, through each way
// a store is reached: a scan, its changes from a cursor, the list's answers,
// a file read from the disk held to its version, a growing file read past the
// size its get gave, and a token that names no path.

import (
	"bytes"
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/blobs"
	"tinyshed.org/tinystore/internal/ways"
)

// beside a store the way opens, the folder movies of the test's directory
func besideAFolder(t *testing.T, way ways.Way) (*tinystore.Store, string) {
	t.Helper()
	dir := t.TempDir()
	folder := filepath.Join(dir, "movies")
	if err := os.MkdirAll(folder, 0o700); err != nil {
		t.Fatal(err)
	}
	store := way.Open(t, dir)
	t.Cleanup(func() { _ = store.Close() })
	return store, folder
}

// movies on the test's folder: by its path, or over a token the server's own.
func movies(t *testing.T, way ways.Way, store *tinystore.Store, folder string, opts ...blobs.DirectoryOption) *blobs.Directory {
	t.Helper()
	if way.Token {
		folder = ""
	}
	return ways.Must(blobs.OpenDirectory(store, "movies", folder, opts...))
}

func write(t *testing.T, folder, path string, data []byte, at int64) {
	t.Helper()
	file := filepath.Join(folder, filepath.FromSlash(path))
	if err := os.MkdirAll(filepath.Dir(file), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(file, data, 0o600); err != nil {
		t.Fatal(err)
	}
	moment := time.Unix(at, 0)
	if err := os.Chtimes(file, moment, moment); err != nil {
		t.Fatal(err)
	}
}

func told(page blobs.Changes) string {
	var out []string
	for _, item := range page.Items {
		out = append(out, item.Path+" "+string(item.Change))
	}
	return strings.Join(out, ", ")
}

func TestAScanListsTheFolderItsChangesFollowACursorAndAReadComesFromTheDisk(t *testing.T) {
	ways.EachWay(t, func(t *testing.T, way ways.Way) {
		ctx := context.Background()
		store, folder := besideAFolder(t, way)
		write(t, folder, "Dune (2021)/Dune.mkv", []byte("spice must flow"), 1_700_000_000)
		write(t, folder, "Heat.mkv", []byte("heat"), 1_700_000_000)
		films := movies(t, way, store, folder)
		scan := ways.Must(films.Scan(ctx))
		if scan != (blobs.Scan{Added: 2, Count: 2, Size: 19}) {
			t.Fatalf("scan %+v", scan)
		}
		first := ways.Must(films.Changes(ctx))
		if got := told(first); got != "Dune (2021)/Dune.mkv added, Heat.mkv added" {
			t.Fatalf("changes %s", got)
		}
		film, found := ways.Must2(films.Get(ctx, "Dune (2021)/Dune.mkv"))
		if !found || !strings.HasPrefix(film.ETag, `"d-f-`) || film.ContentType != "application/octet-stream" {
			t.Fatalf("get %+v", film.FileInfo)
		}
		if got := readAll(t, film); string(got) != "spice must flow" {
			t.Fatalf("read %q", got)
		}
		page := ways.Must(films.Folder("Dune (2021)").List(ctx))
		if len(page.Files) != 1 || page.Files[0].Path != "Dune.mkv" {
			t.Fatalf("list %+v", page)
		}

		// a file past one message is read from the disk as it is read, held to its get's version
		write(t, folder, "Big.mkv", bytes.Repeat([]byte("x"), 3<<20), 1_700_000_000)
		ways.Must(films.Scan(ctx))
		big, _ := ways.Must2(films.Get(ctx, "Big.mkv"))
		write(t, folder, "Big.mkv", bytes.Repeat([]byte("y"), 3<<20), 1_700_000_200)
		_, err := big.Bytes()
		is(t, err, tinystore.ErrConflict)
		_ = big.Close()
		if err := os.Remove(filepath.Join(folder, "Big.mkv")); err != nil {
			t.Fatal(err)
		}
		ways.Must(films.Scan(ctx))
		before := ways.Must(films.Changes(ctx, blobs.After(first.Next)))

		if err := os.Remove(filepath.Join(folder, "Heat.mkv")); err != nil {
			t.Fatal(err)
		}
		write(t, folder, "Dune (2021)/Dune.mkv", []byte("spice must flow, longer"), 1_700_000_100)
		scan = ways.Must(films.Scan(ctx))
		if scan.Changed != 1 || scan.Removed != 1 || scan.Count != 1 {
			t.Fatalf("scan %+v", scan)
		}
		since := ways.Must(films.Changes(ctx, blobs.After(before.Next)))
		if got := told(since); got != "Dune (2021)/Dune.mkv changed, Heat.mkv removed" || since.Items[1].Info != nil {
			t.Fatalf("changes %s", got)
		}
		after := ways.Must(films.Changes(ctx, blobs.After(since.Next)))
		if len(after.Items) != 0 || after.Next != since.Next {
			t.Fatalf("after %+v", after)
		}
		if _, found := ways.Must2(films.Get(ctx, "nothing.mkv")); found {
			t.Fatal("a get read a path no scan listed")
		}
		if err := films.Forget(ctx); err != nil {
			t.Fatal(err)
		}
		if usage := ways.Must(films.Usage(ctx)); usage.Count != 0 {
			t.Fatalf("usage %+v", usage)
		}
	})
}

func TestAGrowingDirectorysReadGoesPastTheSizeItsGetGave(t *testing.T) {
	ways.EachWay(t, func(t *testing.T, way ways.Way) {
		ctx := context.Background()
		store, folder := besideAFolder(t, way)
		write(t, folder, "rec.ts", []byte("0123"), 1_700_000_000)
		live := movies(t, way, store, folder, blobs.Growing())
		ways.Must(live.Scan(ctx))
		recording, _ := ways.Must2(live.Get(ctx, "rec.ts"))
		defer recording.Close()
		file, err := os.OpenFile(filepath.Join(folder, "rec.ts"), os.O_APPEND|os.O_WRONLY, 0)
		if err != nil {
			t.Fatal(err)
		}
		_, _ = file.WriteString("4567")
		_ = file.Close()
		if got := readAll(t, recording); string(got) != "01234567" || recording.Size != 8 {
			t.Fatalf("read %q, size %d", got, recording.Size)
		}
	})
}

func TestAConnectionThatCameWithATokenNamesNoPath(t *testing.T) {
	ways.EachWay(t, func(t *testing.T, way ways.Way) {
		if !way.Token {
			t.Skip("a connection of this machine names a path")
		}
		ctx := context.Background()
		store, folder := besideAFolder(t, way)
		named := ways.Must(blobs.OpenDirectory(store, "elsewhere", folder))
		_, err := named.Usage(ctx)
		is(t, err, tinystore.ErrInvalid)
		missing := ways.Must(blobs.OpenDirectory(store, "photos", ""))
		_, err = missing.Usage(ctx)
		is(t, err, tinystore.ErrInvalid)
	})
}
