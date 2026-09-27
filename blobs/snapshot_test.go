package blobs

import (
	"bytes"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
)

// a snapshot copies blobs.db and hard-links every file its copy names: no
// byte is copied, the links keep their bytes after the objects go, and each
// linked file is a snapshot file of its own that a backup stores as it is
func TestASnapshotLinksFilesAndCopiesTheDatabase(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	film, photo := randomBytes(1, 3<<20), randomBytes(2, 64<<10)
	mustPut(t, media, "film", film)
	mustPut(t, media, "photo", photo)
	mustPut(t, media, "icon", randomBytes(3, 100))
	if _, err := media.Copy(t.Context(), "film", "film-copy"); err != nil {
		t.Fatal(err)
	}

	snapshot, err := s.runtime.Snapshot(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	defer snapshot.Remove()
	var names []string
	for _, file := range snapshot.Files {
		names = append(names, file.Name)
		if file.Engine != "blobs" || file.Stored != strings.HasPrefix(file.Name, "blobs/objects/") {
			t.Fatalf("a snapshot file %+v", file)
		}
	}
	if len(names) != 3 || !slices.Contains(names, "blobs/blobs.db") {
		t.Fatalf("the snapshot holds %v", names)
	}
	for _, key := range []string{"film", "film-copy"} {
		live, statErr := os.Stat(s.pathOf(t, media, key))
		if statErr != nil {
			t.Fatal(statErr)
		}
		_, name := objectName(mustContent(t, media, key))
		linked, statErr := os.Stat(filepath.Join(snapshot.Dir, dirName, name))
		if statErr != nil || !os.SameFile(live, linked) {
			t.Fatalf("the snapshot's %s is not a link of the live file: %v", key, statErr)
		}
	}

	if err = media.Clear(t.Context()); err != nil {
		t.Fatal(err)
	}
	s.maintain(t)
	s.mustHoldFiles(t, 0)
	kept := 0
	for _, file := range snapshot.Files[1:] {
		data, err := os.ReadFile(filepath.Join(snapshot.Dir, filepath.FromSlash(file.Name)))
		if err != nil || !bytes.Equal(data, film) && !bytes.Equal(data, photo) {
			t.Fatalf("%s after the objects went: %d bytes, %v", file.Name, len(data), err)
		}
		kept++
	}
	if kept != 2 {
		t.Fatalf("the snapshot kept %d files", kept)
	}
}

func mustContent(t *testing.T, bucket *Bucket, key string) int64 {
	t.Helper()
	found, err := bucket.lookup(t.Context(), call{path: bucket.folder + key})
	if err != nil || found == nil {
		t.Fatal(found, err)
	}
	return found.content
}

// collection waits for a snapshot: while one links files, the files that
// commits leave without names stay, and maintenance removes them after it
func TestCollectionWaitsForASnapshot(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	mustPut(t, media, "film", randomBytes(1, 1<<20))

	s.collection.Lock()
	if err := media.Delete(t.Context(), "film"); err != nil {
		t.Fatal(err)
	}
	mustBeAbsent(t, media, "film")
	if done := s.maintain(t); done.Removed != 0 {
		t.Fatalf("maintenance removed %d files while a snapshot linked them", done.Removed)
	}
	if files := s.filesIn(t, objectsDir); len(files) != 1 {
		t.Fatalf("a snapshot's file went while it was linked: %v", files)
	}
	s.collection.Unlock()

	if done := s.maintain(t); done.Removed != 1 {
		t.Fatalf("maintenance after the snapshot removed %d files", done.Removed)
	}
	s.mustHoldFiles(t, 0)
}

// a file the snapshot's copy names that has gone missing is left out, and the
// store restored from it reports it as the live one does
func TestASnapshotLeavesOutAMissingFile(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	mustPut(t, media, "film", randomBytes(1, 1<<20))
	if err := os.Remove(s.pathOf(t, media, "film")); err != nil {
		t.Fatal(err)
	}
	snapshot, err := s.runtime.Snapshot(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	defer snapshot.Remove()
	if len(snapshot.Files) != 1 {
		t.Fatalf("the snapshot holds %+v", snapshot.Files)
	}
}
