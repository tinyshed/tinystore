package blobs

import (
	"errors"
	"os"
	"runtime"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

// the files arriving together in a directory share its sync, and each waits
// for a sync that began after it asked
func TestADirectorySyncIsSharedByTheFilesArrivingTogether(t *testing.T) {
	var shared dirSync
	var flushes, running atomic.Int32
	var asked sync.Map // caller → the flushes begun when it asked
	flush := func() error {
		if running.Add(1) != 1 {
			t.Error("two syncs of one directory ran at once")
		}
		flushes.Add(1)
		time.Sleep(time.Millisecond)
		running.Add(-1)
		return nil
	}
	var callers sync.WaitGroup
	for caller := range 64 {
		callers.Go(func() {
			asked.Store(caller, flushes.Load())
			if err := shared.run(flush); err != nil {
				t.Error(err)
			}
			if before, _ := asked.Load(caller); flushes.Load() <= before.(int32) {
				t.Error("a caller returned before a sync that began after it asked")
			}
		})
	}
	callers.Wait()
	if flushes.Load() >= 64 {
		t.Fatalf("64 callers ran %d syncs", flushes.Load())
	}
	failing := errors.New("the disk failed")
	if err := shared.run(func() error { return failing }); !errors.Is(err, failing) {
		t.Fatalf("a failed sync: %v", err)
	}
}

// the disk's free space is what the system says, where it says
func TestFreeSpaceIsTheDisks(t *testing.T) {
	free, err := freeSpace(t.TempDir())
	if err != nil {
		t.Skipf("this system does not say: %v", err)
	}
	if free <= 0 || free >= 1<<62 {
		t.Fatalf("a disk with %d bytes free", free)
	}
}

// a file another program holds without sharing its deletion is not removed
// while it holds it: the write that freed it succeeds, and maintenance
// removes the file once it is let go
func TestAFileHeldElsewhereIsRemovedLater(t *testing.T) {
	if runtime.GOOS != "windows" {
		t.Skip("only Windows refuses to remove a file another program holds")
	}
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	mustPut(t, media, "film", randomBytes(1, 1<<20))
	held, err := os.Open(s.pathOf(t, media, "film")) // os.Open shares no deletion on Windows
	if err != nil {
		t.Fatal(err)
	}
	if err = media.Delete(t.Context(), "film"); err != nil {
		t.Fatalf("a Delete of a file another program holds: %v", err)
	}
	mustBeAbsent(t, media, "film")
	if done := s.maintain(t); done.Removed != 0 || len(s.filesIn(t, objectsDir)) != 1 {
		t.Fatalf("maintenance removed %d files while one was held", done.Removed)
	}
	if err = held.Close(); err != nil {
		t.Fatal(err)
	}
	if done := s.maintain(t); done.Removed != 1 {
		t.Fatalf("maintenance removed %d files once the file was let go", done.Removed)
	}
	s.mustHoldFiles(t, 0)
}
