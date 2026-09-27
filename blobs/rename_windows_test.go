package blobs

import (
	"bytes"
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"
)

func TestARenameRetriesAfterAWindowsScannerLetsGo(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	data := randomBytes(1, 64<<10)
	upload, err := media.Create(t.Context(), "held")
	if err != nil {
		t.Fatal(err)
	}
	defer upload.Abort()
	if _, err = upload.Write(data); err != nil {
		t.Fatal(err)
	}
	held, err := os.Open(filepath.Join(s.Store.dir, uploadName(upload.id)))
	if err != nil {
		t.Fatal(err)
	}
	defer held.Close()
	finished := make(chan error, 1)
	go func() {
		_, commitErr := upload.Commit(t.Context())
		finished <- commitErr
	}()
	deadline := time.Now().Add(5 * time.Second)
	for s.retries.Load() == 0 && time.Now().Before(deadline) {
		runtime.Gosched()
	}
	if err = held.Close(); err != nil {
		t.Fatal(err)
	}
	if err = <-finished; err != nil || s.retries.Load() == 0 {
		t.Fatalf("rename after scanner releases: retries %d, %v", s.retries.Load(), err)
	}
	mustHold(t, media, "held", data)
}

func TestAHeldRenameExhaustsRetriesAndRecovers(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	data := randomBytes(2, 64<<10)
	upload, err := media.Create(t.Context(), "held")
	if err != nil {
		t.Fatal(err)
	}
	defer upload.Abort()
	if _, err = upload.Write(data); err != nil {
		t.Fatal(err)
	}
	held, err := os.Open(filepath.Join(s.Store.dir, uploadName(upload.id)))
	if err != nil {
		t.Fatal(err)
	}
	defer held.Close()
	_, err = upload.Commit(t.Context())
	if err == nil || !heldElsewhere(err) || s.retries.Load() != renameTries-1 {
		t.Fatalf("held rename: retries %d, %v", s.retries.Load(), err)
	}
	mustBeAbsent(t, media, "held")
	if err = held.Close(); err != nil {
		t.Fatal(err)
	}
	s.maintain(t)
	if left := s.filesIn(t, uploadsDir); len(left) != 0 {
		t.Fatalf("uploads left after the scanner closed: %v", left)
	}
	if _, err = media.Put(t.Context(), "held", bytes.NewReader(data)); err != nil {
		t.Fatal(err)
	}
	mustHold(t, media, "held", data)
}
