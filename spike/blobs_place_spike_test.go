package spike

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"fmt"
	"io"
	"math/rand/v2"
	"os"
	"path/filepath"
	"strconv"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// the tables of a blobs.db the round needs: an object names a content, whose
// bytes are a row of bodies when inline, a file of its own otherwise
var blobsSchema = []string{
	`create table objects (
		bucket  integer not null,
		path    text    not null,
		content integer not null,
		size    integer not null,
		etag    blob    not null,
		primary key (bucket, path)
	) strict, without rowid`,
	`create table bodies (id integer primary key, bytes blob not null) strict`,
	`create table uploads (id integer primary key, path text not null, started integer not null) strict`,
}

const (
	blobsInsertBody   = `insert into bodies (id, bytes) values (?1, ?2)`
	blobsInsertObject = `insert or replace into objects (bucket, path, content, size, etag) values (1, ?1, ?2, ?3, ?4)`
	blobsInsertUpload = `insert into uploads (id, path, started) values (?1, ?2, 0)`
	blobsDropUpload   = `delete from uploads where id = ?1`
	blobsReadInline   = `select b.bytes from objects o join bodies b on b.id = o.content
		where o.bucket = 1 and o.path = ?1`
	blobsReadPlace = `select content, size from objects where bucket = 1 and path = ?1`
)

// blobsOpenDB opens a blobs.db through internal/sqlite, as an engine would,
// with 4 KiB pages and eight readers
func blobsOpenDB(t *testing.T, dir string) (*sqlite.File, string) {
	t.Helper()
	ctx := t.Context()
	path := filepath.Join(dir, "blobs.db")
	file, err := sqlite.Open(ctx, path, sqlite.Config{Readers: 8, PageSize: 4096})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = file.Close() })
	err = file.Update(ctx, func(tx *sql.Tx) error {
		for _, statement := range blobsSchema {
			if _, execErr := tx.ExecContext(ctx, statement); execErr != nil {
				return execErr
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	return file, path
}

// blobsStore is one place's objects: a blobs.db, and the files beside it
type blobsStore struct {
	db      *sqlite.File
	dbPath  string
	tree    *blobsTree
	root    *os.Root
	syncers sync.Map // a directory → its *blobsDirSyncer
	nextID  atomic.Int64
}

func blobsOpenStore(t *testing.T) *blobsStore {
	t.Helper()
	dir := blobsDir(t)
	db, dbPath := blobsOpenDB(t, dir)
	root, err := os.OpenRoot(dir)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = root.Close() })
	return &blobsStore{db: db, dbPath: dbPath, tree: &blobsTree{root: dir}, root: root}
}

// putInline writes an object and its bytes in one grouped commit
func (s *blobsStore) putInline(ctx context.Context, path string, data []byte) error {
	id := s.nextID.Add(1)
	sum := sha256.Sum256(data)
	return s.db.UpdateGrouped(ctx, len(data), func(w sqlite.Writer) error {
		if _, err := w.ExecContext(ctx, blobsInsertBody, id, data); err != nil {
			return err
		}
		_, err := w.ExecContext(ctx, blobsInsertObject, path, id, len(data), sum[:])
		return err
	})
}

// blobsFileWay is how a file's Put makes its name durable and whether a row
// of uploads is committed before its first byte
type blobsFileWay struct {
	syncDir, shareSync, uploadRow bool
}

// putFile writes an object's bytes to a file of their own, syncs it and its
// directory as the way says, then commits the object's row
func (s *blobsStore) putFile(ctx context.Context, path string, data []byte, way blobsFileWay) error {
	id := s.nextID.Add(1)
	if way.uploadRow {
		err := s.db.UpdateGrouped(ctx, 0, func(w sqlite.Writer) error {
			_, err := w.ExecContext(ctx, blobsInsertUpload, id, path)
			return err
		})
		if err != nil {
			return err
		}
	}
	sum, err := s.writeFile(id, data, way)
	if err != nil {
		return err
	}
	return s.db.UpdateGrouped(ctx, 0, func(w sqlite.Writer) error {
		if way.uploadRow {
			if _, err := w.ExecContext(ctx, blobsDropUpload, id); err != nil {
				return err
			}
		}
		_, err := w.ExecContext(ctx, blobsInsertObject, path, id, len(data), sum)
		return err
	})
}

func (s *blobsStore) writeFile(id int64, data []byte, way blobsFileWay) ([]byte, error) {
	dir, err := s.tree.dirOf(id)
	if err != nil {
		return nil, err
	}
	sum := blobsHash()
	_, name := blobsName(id)
	if err = blobsWrite(filepath.Join(s.tree.root, name), data, sum); err != nil {
		return nil, err
	}
	return sum.Sum(nil), s.syncName(dir, way)
}

// syncName makes a new file's name durable as the way says: its directory
// synced after it, alone or shared, or not at all
func (s *blobsStore) syncName(dir string, way blobsFileWay) error {
	switch {
	case way.shareSync:
		syncer, _ := s.syncers.LoadOrStore(dir, &blobsDirSyncer{})
		if shared, ok := syncer.(*blobsDirSyncer); ok {
			return shared.sync(dir)
		}
	case way.syncDir:
		return blobsSyncDir(dir)
	}
	return nil
}

// openInline reads an inline object whole and checks it before its first byte
func (s *blobsStore) openInline(ctx context.Context, path string) error {
	var data []byte
	err := s.db.Lookup(ctx, func(r sqlite.Reader) error {
		return sqlite.QueryRow(ctx, r, blobsReadInline, path).Scan(&data)
	})
	if err != nil {
		return err
	}
	sha256.Sum256(data)
	return nil
}

// openFile looks an object up, opens its file through the root and reads it
// whole, hashing it as a checked whole read does
func (s *blobsStore) openFile(ctx context.Context, path string) error {
	var id, size int64
	err := s.db.Lookup(ctx, func(r sqlite.Reader) error {
		return sqlite.QueryRow(ctx, r, blobsReadPlace, path).Scan(&id, &size)
	})
	if err != nil {
		return err
	}
	_, name := blobsName(id)
	file, err := s.root.Open(name)
	if err != nil {
		return err
	}
	read, err := io.Copy(blobsHash(), file)
	if err == nil && read != size {
		err = fmt.Errorf("read %d of %d bytes", read, size)
	}
	return errorsJoin(err, file.Close())
}

// blobsDirSyncer shares a directory's sync among the writes that finish while
// one runs: each waits for a sync that began after its file's name existed
type blobsDirSyncer struct {
	mu            sync.Mutex
	cond          *sync.Cond
	syncing       bool
	begun, synced uint64
	err           error
}

func (s *blobsDirSyncer) sync(dir string) error {
	return s.run(func() error { return blobsSyncDir(dir) })
}

// run waits for a run of flush that began after it was called, starting one
// when none runs
func (s *blobsDirSyncer) run(flush func() error) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.cond == nil {
		s.cond = sync.NewCond(&s.mu)
	}
	want := s.begun + 1
	for s.synced < want {
		if s.syncing {
			s.cond.Wait()
			continue
		}
		s.syncing = true
		s.begun++
		mine := s.begun
		s.mu.Unlock()
		err := flush()
		s.mu.Lock()
		s.syncing, s.synced, s.err = false, mine, err
		s.cond.Broadcast()
	}
	return s.err
}

// TestBlobsPlaces writes and reads objects of 4 to 256 KiB from 1, 16 and 128
// callers, kept inline as rows of blobs.db or as files of their own synced with
// their directories, and reports what the inline file holds and what copying
// it costs
func TestBlobsPlaces(t *testing.T) {
	kvMeasuring(t)
	for _, size := range []int{4 << 10, 16 << 10, 64 << 10, 256 << 10} {
		for _, place := range []string{"inline", "file"} {
			store := blobsOpenStore(t)
			for _, callers := range []int{1, 16, 128} {
				blobsPlaceLoad(t, store, place, size, callers)
			}
			if place == "inline" {
				blobsReportDB(t, store, size)
			}
		}
	}
}

func blobsPlaceLoad(t *testing.T, store *blobsStore, place string, size, callers int) {
	t.Helper()
	ctx := t.Context()
	data := blobsBytes(uint64(size), size)
	label := fmt.Sprintf("%-6s %7s %3d callers", place, blobsSize(size), callers)
	written := make([][]string, callers)
	peak := blobsWatchFile(store.dbPath + "-wal")
	puts := blobsLoad(t, callers, blobsSeconds(), func(caller, n int) error {
		path := fmt.Sprintf("c%d/%d/o%d", callers, caller, n)
		written[caller] = append(written[caller], path)
		if place == "inline" {
			return store.putInline(ctx, path, data)
		}
		return store.putFile(ctx, path, data, blobsFileWay{shareSync: true})
	})
	t.Logf("%s  Put  %s  WAL peak %5.1f MiB", label, puts, float64(peak())/(1<<20))
	opens := blobsLoad(t, callers, blobsSeconds(), func(caller, n int) error {
		mine := written[caller]
		path := mine[rand.IntN(len(mine))]
		if place == "inline" {
			return store.openInline(ctx, path)
		}
		return store.openFile(ctx, path)
	})
	t.Logf("%s  Open %s", label, opens)
}

// blobsWatchFile samples a file's size every 20 ms until the returned function
// is called, which answers the largest size seen
func blobsWatchFile(path string) func() int64 {
	var peak atomic.Int64
	stop := make(chan struct{})
	done := make(chan struct{})
	go func() {
		defer close(done)
		for {
			if info, err := os.Stat(path); err == nil && info.Size() > peak.Load() {
				peak.Store(info.Size())
			}
			select {
			case <-stop:
				return
			case <-time.After(20 * time.Millisecond):
			}
		}
	}()
	return func() int64 {
		close(stop)
		<-done
		return peak.Load()
	}
}

// blobsReportDB divides the inline file by object and times copying it whole
// with VACUUM INTO, as a snapshot copies every engine's file
func blobsReportDB(t *testing.T, store *blobsStore, size int) {
	t.Helper()
	ctx := t.Context()
	for _, object := range jobsDivide(t, store.db) {
		if object.bytes > 1<<20 {
			t.Logf("inline %7s   %-8s %8.1f MiB, payload %4.1f %%, unused %4.1f %%", blobsSize(size), object.name,
				float64(object.bytes)/(1<<20), 100*float64(object.payload)/float64(object.bytes),
				100*float64(object.unused)/float64(object.bytes))
		}
	}
	into := store.dbPath + ".copy"
	began := time.Now()
	if err := blobsVacuumInto(ctx, store.dbPath, into); err != nil {
		t.Fatal(err)
	}
	copied := kvFileBytes(into)
	t.Logf("inline %7s   VACUUM INTO %7.1f MiB in %s, %5.2f GB/s", blobsSize(size), float64(copied)/(1<<20),
		jobsMillis(time.Since(began)), float64(copied)/time.Since(began).Seconds()/1e9)
	_ = os.Remove(into)
}

// blobsVacuumInto copies a database whole from a read-only connection of its
// own, as internal/sqlite's Snapshot does, while its writer may go on
func blobsVacuumInto(ctx context.Context, path, into string) error {
	copier, err := sql.Open("sqlite", blobsReadOnly(path))
	if err != nil {
		return err
	}
	_, err = copier.ExecContext(ctx, `vacuum into ?1`, into)
	return errorsJoin(err, copier.Close())
}

func blobsReadOnly(path string) string {
	return "file:" + filepath.ToSlash(path) + "?mode=ro&_pragma=busy_timeout(5000)"
}

// TestBlobsUploadRow times a file's Put of 64 KiB to 4 MiB, from one and from
// sixteen callers, with a row of uploads committed before its first byte and
// without one
func TestBlobsUploadRow(t *testing.T) {
	kvMeasuring(t)
	ctx := t.Context()
	for _, size := range []int{64 << 10, 256 << 10, 1 << 20, 4 << 20} {
		data := blobsBytes(uint64(size), size)
		for _, row := range []bool{false, true} {
			store := blobsOpenStore(t)
			for _, callers := range []int{1, 16} {
				way := blobsFileWay{shareSync: true, uploadRow: row}
				puts := blobsLoad(t, callers, blobsSeconds(), func(caller, n int) error {
					return store.putFile(ctx, fmt.Sprintf("c%d/%d/o%d", callers, caller, n), data, way)
				})
				t.Logf("file %7s  a row of uploads first %-5v  %3d callers  Put %s", blobsSize(size), row, callers, puts)
			}
		}
	}
}

// TestBlobsDirSync writes 64 KiB files into one directory from 1, 16 and 128
// callers: each file synced alone, then with its directory synced after it,
// then with the directory's sync shared by the files that finish together
func TestBlobsDirSync(t *testing.T) {
	kvMeasuring(t)
	data := blobsBytes(64, 64<<10)
	for _, way := range []struct {
		name string
		way  blobsFileWay
	}{
		{"the file's sync", blobsFileWay{}},
		{"and its directory's", blobsFileWay{syncDir: true}},
		{"a directory sync shared", blobsFileWay{shareSync: true}},
	} {
		for _, callers := range []int{1, 16, 128} {
			store := blobsOpenStore(t)
			dir := filepath.Join(store.tree.root, "one")
			if err := os.Mkdir(dir, 0o750); err != nil {
				t.Fatal(err)
			}
			writes := blobsLoad(t, callers, blobsSeconds(), func(int, int) error {
				name := filepath.Join(dir, strconv.FormatInt(store.nextID.Add(1), 16))
				if err := blobsWrite(name, data, blobsHash()); err != nil {
					return err
				}
				return store.syncName(dir, way.way)
			})
			t.Logf("64 KiB files, %-24s %3d callers  %s", way.name, callers, writes)
		}
	}
}
