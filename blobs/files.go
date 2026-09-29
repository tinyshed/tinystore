package blobs

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strconv"
	"sync"
	"time"

	"github.com/tinyshed/tinystore"
)

// where a content's bytes lie before its commit, and after it
const (
	uploadsDir = "uploads"
	objectsDir = "objects"
)

// uploadName and objectName are a content's files under blobs/, named by its
// id in hex, a directory of objects/ for each 4,096 ids:
//
//	0x1a3f07 → uploads/1a3f07 → objects/1a3/1a3f07
func uploadName(id int64) string {
	return filepath.Join(uploadsDir, strconv.FormatInt(id, 16))
}

func objectName(id int64) (dir, name string) {
	dir = objectDir(id >> idsPerDir)
	return dir, filepath.Join(dir, strconv.FormatInt(id, 16))
}

// objectDir is the directory of objects/ that holds the ids of one fan
func objectDir(fan int64) string {
	return filepath.Join(objectsDir, strconv.FormatInt(fan, 16))
}

// idOf reads an id back from its file's name; a name the engine did not give
// is no id
func idOf(name string) (int64, bool) {
	id, err := strconv.ParseInt(name, 16, 64)
	return id, err == nil && id > 0 && strconv.FormatInt(id, 16) == name
}

// makeDir creates a directory of objects/ the first time a file goes there. It
// syncs objects/ so that the directory's name is durable before any file in it
// is.
func (s *Store) makeDir(dir string) error {
	if _, made := s.made.Load(dir); made {
		return nil
	}
	if err := s.root.Mkdir(dir, 0o750); err != nil && !errors.Is(err, fs.ErrExist) {
		return fmt.Errorf("blobs: create %s: %w", dir, err)
	}
	if err := s.syncShared(objectsDir); err != nil {
		return err
	}
	s.made.Store(dir, true)
	return nil
}

// rename moves an upload's file into objects/, trying again a few times when
// a scanner that opened the new file without sharing it refuses the rename
func (s *Store) rename(from, to string) error {
	for try := 0; ; try++ {
		err := s.renameFile(from, to)
		if err == nil {
			return nil
		}
		if !heldElsewhere(err) || try == renameTries-1 {
			return fmt.Errorf("blobs: publish %s: %w", to, err)
		}
		s.retries.Add(1)
		time.Sleep(time.Millisecond << try)
	}
}

func (s *Store) renameFile(from, to string) error {
	s.nameCalls.Lock()
	defer s.nameCalls.Unlock()
	return s.root.Rename(from, to)
}

func (s *Store) createUpload(id int64) (*os.File, error) {
	s.nameCalls.Lock()
	defer s.nameCalls.Unlock()
	return s.root.OpenFile(uploadName(id), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o600)
}

// syncShared syncs a directory of blobs/ once for the files that arrive in it
// together: each waits for a sync that began after its own rename
func (s *Store) syncShared(dir string) error {
	shared, _ := s.syncs.LoadOrStore(dir, &dirSync{})
	syncer, ok := shared.(*dirSync)
	if !ok {
		return fmt.Errorf("blobs: the sync of %s is a %T", dir, shared)
	}
	if err := syncer.run(func() error { return syncDirectory(filepath.Join(s.dir, dir)) }); err != nil {
		return fmt.Errorf("blobs: sync %s: %w", dir, err)
	}
	return nil
}

// dirSync shares a directory's sync: a caller waits for a sync that began
// after it called, and starts one when none runs
type dirSync struct {
	mu            sync.Mutex
	done          sync.Cond
	running       bool
	begun, synced uint64
	err           error
}

func (d *dirSync) run(flush func() error) error {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.done.L == nil {
		d.done.L = &d.mu
	}
	want := d.begun + 1
	for d.synced < want {
		if d.running {
			d.done.Wait()
			continue
		}
		d.running = true
		d.begun++
		mine := d.begun
		d.mu.Unlock()
		err := flush()
		d.mu.Lock()
		d.running, d.synced, d.err = false, mine, err
		d.done.Broadcast()
	}
	return d.err
}

// removeFile removes a content's file, which no key names any more. A file
// already gone counts as removed.
func (s *Store) removeFile(id int64) error {
	_, name := objectName(id)
	if err := s.root.Remove(name); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return fmt.Errorf("blobs: remove %s: %w", name, err)
	}
	return nil
}

// letFilesGo removes the files of contents a commit left without names. While a
// snapshot links files, or where a removal fails, it leaves them to
// maintenance.
func (s *Store) letFilesGo(ids []int64) {
	if len(ids) == 0 || !s.collection.TryRLock() {
		return
	}
	defer s.collection.RUnlock()
	for _, id := range ids {
		if err := s.removeFile(id); err != nil {
			s.removals.observe(s.now(), err)
		}
	}
}

// checkFree refuses an upload that would leave less than KeepFree of the disk
// free once need more bytes are written. A disk that does not say what it has
// free is not checked.
func (s *Store) checkFree(need int64) error {
	if s.keepFree < 0 {
		return nil
	}
	free, err := freeSpace(s.dir)
	if err != nil || free-need >= s.keepFree {
		return nil //nolint:nilerr // an unknown free space refuses no upload
	}
	return fmt.Errorf("%w: %d bytes of the disk free, %d more to write, and uploads keep %d free",
		tinystore.ErrLimit, free, need, s.keepFree)
}
