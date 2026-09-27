package blobs

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strconv"
	"sync"
	"testing"
)

// the objects of the abrupt exit: every one was answered before the exit,
// the even ones inline and the odd ones in files
const crashObjects = 64

func crashBytes(n int) []byte {
	if n%2 == 0 {
		return randomBytes(uint64(n), 100)
	}
	return randomBytes(uint64(n), 64<<10)
}

// a Put that returned is there after an exit that closes nothing, inline and
// in a file: it returns only once its bytes are synced and its row committed
func TestAPutThatReturnedSurvivesAnAbruptExit(t *testing.T) {
	dir := t.TempDir()
	runChild(t, "^TestAbruptExitHelper$", dir, "")

	s := openTestStore(t, dir)
	media := openTestBucket(t, s, "crash")
	for n := range crashObjects {
		mustHold(t, media, strconv.Itoa(n), crashBytes(n))
	}
	s.mustHoldFiles(t, crashObjects/2)
}

// TestAbruptExitHelper runs in the child: it puts from many goroutines at
// once, so that the objects commit in groups, and exits without closing
func TestAbruptExitHelper(t *testing.T) {
	dir := os.Getenv("TINYSTORE_BLOBS_CRASH_DIR")
	if dir == "" {
		t.Skip("subprocess only")
	}
	s := openTestStore(t, dir)
	media := openTestBucket(t, s, "crash")
	var wg sync.WaitGroup
	errs := make([]error, crashObjects)
	for n := range crashObjects {
		wg.Go(func() {
			_, errs[n] = media.Put(context.Background(), strconv.Itoa(n), bytes.NewReader(crashBytes(n)))
		})
	}
	wg.Wait()
	exitOn(errors.Join(errs...))
}

// the moments of an upload replacing an object that the child ends itself at
var crashMoments = map[string]step{
	"written": stepWritten, "synced": stepSynced, "published": stepPublished, "committed": stepCommitted,
}

// an exit at every step of an upload that replaces an object leaves the old
// object whole or the new one, no row naming bytes that are not there, and no
// file that the next Open and one maintenance keep
func TestAnExitAtEveryStepOfAnUploadLeavesNothingBehind(t *testing.T) {
	for _, moment := range []string{"writing", "written", "synced", "published", "committed"} {
		t.Run(moment, func(t *testing.T) {
			dir := t.TempDir()
			runChild(t, "^TestExitAtAStepHelper$", dir, moment)

			s := openTestStore(t, dir)
			media := openTestBucket(t, s, "crash")
			want := randomBytes(1, 3<<20)
			if moment == "committed" {
				want = randomBytes(2, 3<<20)
			}
			mustHold(t, media, "doc", want)
			opened := 1
			if moment == "committed" {
				opened = 2 // the replaced file, which its commit had left to remove
			}
			s.mustHoldFiles(t, opened)
			s.maintain(t)
			s.mustHoldFiles(t, 1)
			mustHold(t, media, "doc", want)
		})
	}
}

// TestExitAtAStepHelper runs in the child: it puts an object, then replaces
// it with another and ends the process at the moment the parent names
func TestExitAtAStepHelper(t *testing.T) {
	dir := os.Getenv("TINYSTORE_BLOBS_CRASH_DIR")
	if dir == "" {
		t.Skip("subprocess only")
	}
	s := openTestStore(t, dir)
	media := openTestBucket(t, s, "crash")
	mustPut(t, media, "doc", randomBytes(1, 3<<20))
	moment := os.Getenv("TINYSTORE_BLOBS_CRASH_AT")
	stream := io.Reader(bytes.NewReader(randomBytes(2, 3<<20)))
	if moment == "writing" {
		stream = &exitingReader{Reader: stream, left: 2 << 20}
	}
	s.crashAt = func(at step) {
		if crashMoments[moment] == at {
			os.Exit(0)
		}
	}
	_, err := media.Put(context.Background(), "doc", stream)
	exitOn(errors.Join(err, fmt.Errorf("the child reached no moment %q", moment)))
}

// exitingReader ends the process once it has yielded left bytes, as a
// process that dies halfway through an upload does
type exitingReader struct {
	io.Reader
	left int
}

func (e *exitingReader) Read(p []byte) (int, error) {
	if e.left <= 0 {
		os.Exit(0)
	}
	n, err := e.Reader.Read(p[:min(len(p), e.left)])
	e.left -= n
	return n, err
}

// the next Open empties uploads/ and removes a file of objects/ that no
// content names only where an upload may have left one, past the settled
// mark: a file below it, and a name the engine did not give, it leaves alone
func TestOpenRemovesWhatAbandonedUploadsLeft(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	mustPut(t, media, "kept", randomBytes(1, 1<<20))
	mustPut(t, media, "gone", randomBytes(2, 1<<20))
	if err := media.Delete(t.Context(), "gone"); err != nil {
		t.Fatal(err)
	}
	below := s.ids.last
	abandoned, err := s.takeID(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range []int64{below, abandoned} {
		s.writeFile(t, objectName, id)
	}
	s.writeFile(t, func(int64) (string, string) { return objectsDir, filepath.Join(objectsDir, "0", "notes.txt") }, 0)
	s.writeFile(t, func(id int64) (string, string) { return uploadsDir, uploadName(id) }, abandoned+1)
	s.writeFile(t, func(int64) (string, string) { return uploadsDir, filepath.Join(uploadsDir, "stray") }, 0)

	s = s.reopen(t)
	media = openTestBucket(t, s, "media")
	names := s.filesIn(t, objectsDir)
	slices.Sort(names)
	want := []string{strconv.FormatInt(below-1, 16), strconv.FormatInt(below, 16), "notes.txt"}
	if !slices.Equal(names, want) || len(s.filesIn(t, uploadsDir)) != 0 {
		t.Fatalf("after the Open objects/ holds %v, want %v; uploads/ %v", names, want, s.filesIn(t, uploadsDir))
	}
	mustBeAbsent(t, media, "gone")
}

// writeFile writes a file where name puts an id's, as a crash would leave it
func (s *testStore) writeFile(t *testing.T, name func(int64) (string, string), id int64) {
	t.Helper()
	dir, path := name(id)
	if err := os.MkdirAll(filepath.Join(s.Store.dir, filepath.Dir(path)), 0o750); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(s.Store.dir, path), []byte(dir), 0o600); err != nil {
		t.Fatal(err)
	}
}

func runChild(t *testing.T, test, dir, moment string) {
	t.Helper()
	command := exec.CommandContext(t.Context(), os.Args[0], "-test.run="+test)
	command.Env = append(os.Environ(), "TINYSTORE_BLOBS_CRASH_DIR="+dir, "TINYSTORE_BLOBS_CRASH_AT="+moment)
	if output, err := command.CombinedOutput(); err != nil {
		t.Fatalf("child failed: %v\n%s", err, output)
	}
}

func exitOn(err error) {
	if err != nil {
		fmt.Println(err)
		os.Exit(1)
	}
	os.Exit(0)
}
