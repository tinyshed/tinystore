package blobs

import (
	"context"
	"embed"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/admission"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

//go:embed migrations/*.sql
var migrationFiles embed.FS

// the directory this engine claims inside the store's, and its file there
const (
	dirName  = "blobs"
	fileName = "blobs.db"
)

// blobsApplicationID is "TBLB", the SQLite application id that claims a file for this engine
const blobsApplicationID = 0x54424c42

var errClosed = fmt.Errorf("blobs: %w", tinystore.ErrClosed)

// Store is the engine: blobs.db, and the files beside it in blobs/.
type Store struct {
	runtime     *tinystore.Store
	dir         string
	root        *os.Root // blobs/, through which every file is opened, so that a reader shares its deletion
	nameCalls   nameCalls
	file        *sqlite.File
	log         *slog.Logger
	now         func() time.Time
	keepFree    int64
	clearBound  int // objects a Clear deletes in its transaction before it marks instead
	gate        admission.Gate
	writes      admission.Slots
	uploads     admission.Slots
	revision    atomic.Int64
	ids         ids
	live        liveUploads
	made        sync.Map     // a directory of objects/ → true, once it exists
	syncs       sync.Map     // a directory of blobs/ → its *dirSync
	collection  sync.RWMutex // a snapshot holds it while it links the files its copy names
	maintenance chan struct{}
	removals    *quietLog
	retries     atomic.Int64 // renames a scanner made wait, on Windows
	lookedAgain atomic.Int64 // Opens that looked their key up again, the file they found gone
	closing     sync.Once
	closeErr    error
	crashAt     func(step) // nil but in the gates, which end the process at a step of a commit
}

// Open opens blobs/ inside the store: blobs.db and the files beside it. It
// removes what the uploads of a process that died left, and the store closes
// it and, unless it is Manual, runs its maintenance every minute.
func Open(ctx context.Context, store *tinystore.Store, options Options) (*Store, error) {
	dir, release, err := store.Claim(dirName + "/")
	if err != nil {
		return nil, err
	}

	objects, err := openEngine(ctx, store, dir, options)
	if err != nil {
		release()
		return nil, err
	}

	store.EveryEngine("blobs", "blobs maintenance", maintainEvery, objects.maintainInBackground)
	objects.log.Info("opened", "path", dir)
	return objects, nil
}

// openEngine opens the directory and the file, removes what a process that
// died left, and hands the engine to the store
func openEngine(ctx context.Context, store *tinystore.Store, dir string, options Options) (*Store, error) {
	root, err := openDirectory(dir)
	if err != nil {
		return nil, err
	}
	file, err := openFile(ctx, filepath.Join(dir, fileName))
	if err != nil {
		return nil, errors.Join(err, root.Close())
	}
	s := &Store{
		runtime: store, dir: dir, root: root, file: file, log: store.Logger("blobs"), now: store.Now,
		keepFree: options.KeepFree, clearBound: clearAtOnce,
		writes: admission.NewSlots(writeSlots), uploads: admission.NewSlots(uploadSlots),
		ids: ids{held: map[int64]bool{}, doubtful: map[int64]bool{}}, live: liveUploads{set: map[*Upload]bool{}},
		maintenance: make(chan struct{}, 1),
	}
	if s.keepFree == 0 {
		s.keepFree = defaultKeepFree
	}
	s.removals = newQuietLog(s.log, "files no key names wait to be removed")
	s.maintenance <- struct{}{}
	if err = s.recover(ctx); err == nil {
		err = store.Attach(s)
	}
	if err != nil {
		return nil, errors.Join(err, file.Close(), root.Close())
	}
	return s, nil
}

// openDirectory roots every file the engine opens in blobs/, which also keeps
// a path from leaving it, creates uploads/ and objects/ there, and syncs the
// directories that name them
func openDirectory(dir string) (*os.Root, error) {
	root, err := os.OpenRoot(dir)
	if err != nil {
		return nil, fmt.Errorf("blobs: open %s: %w", dir, err)
	}
	for _, sub := range []string{uploadsDir, objectsDir} {
		if err = root.MkdirAll(sub, 0o750); err != nil {
			return nil, errors.Join(fmt.Errorf("blobs: create %s: %w", sub, err), root.Close())
		}
	}
	for _, named := range []string{dir, filepath.Dir(dir)} {
		if err = syncDirectory(named); err != nil {
			return nil, errors.Join(fmt.Errorf("blobs: sync %s: %w", named, err), root.Close())
		}
	}
	return root, nil
}

func openFile(ctx context.Context, path string) (*sqlite.File, error) {
	file, err := sqlite.Open(ctx, path, sqlite.Config{Readers: readers, PageSize: pageSize})
	if err != nil {
		return nil, fmt.Errorf("blobs: open: %w", err)
	}
	scripts, err := fs.Sub(migrationFiles, "migrations")
	if err == nil {
		err = file.Migrate(ctx, blobsApplicationID, scripts)
	}
	if err != nil {
		return nil, errors.Join(fmt.Errorf("blobs: migrate: %w", err), file.Close())
	}
	return file, nil
}

// Close aborts the uploads that have not committed, lets the work in flight
// finish, settles the ids no upload holds any more and closes blobs.db; a
// reader of a file reads on until its own Close. Cancellation stops waiting,
// not the cleanup. The store calls it: an application closes the store instead.
func (s *Store) Close(ctx context.Context) error {
	drained, _ := s.gate.Close()
	s.live.abortAll(errClosed)
	select {
	case <-drained:
	case <-ctx.Done():
		return ctx.Err()
	}
	s.closing.Do(func() {
		cleanup := context.WithoutCancel(ctx)
		s.resolveDoubts(cleanup)
		settleErr := s.settle(cleanup)
		s.closeErr = errors.Join(settleErr, s.file.Close(), s.root.Close())
		s.log.Info("closed")
	})
	return s.closeErr
}

// admit lets one operation in while the store is open; release lets it out
func (s *Store) admit(ctx context.Context) (release func(), err error) {
	if err = s.gate.Enter(ctx, errClosed); err != nil {
		return nil, err
	}
	return s.gate.Leave, nil
}

// admitWrite lets a write in and holds one of the write slots until it is
// answered, so that the writes waiting for a group are bounded
func (s *Store) admitWrite(ctx context.Context) (release func(), err error) {
	leave, err := s.admit(ctx)
	if err != nil {
		return nil, err
	}
	free, err := s.writes.Take(ctx)
	if err != nil {
		leave()
		return nil, err
	}
	return func() {
		free()
		leave()
	}, nil
}

// admitUpload lets an upload in and holds one of the upload slots until it
// ends, so that uploads at once are bounded
func (s *Store) admitUpload(ctx context.Context) (release func(), err error) {
	leave, err := s.admit(ctx)
	if err != nil {
		return nil, err
	}
	free, err := s.uploads.Take(ctx)
	if err != nil {
		leave()
		return nil, err
	}
	return sync.OnceFunc(func() {
		free()
		leave()
	}), nil
}

// reserve holds an operation's bytes in the store's memory before it makes
// them, waiting in arrival order until they fit
func (s *Store) reserve(ctx context.Context, bytes int) (*tinystore.Reservation, error) {
	reserved, err := s.runtime.Reserve(ctx, int64(max(bytes, 1)))
	if errors.Is(err, tinystore.ErrLimit) {
		return nil, fmt.Errorf("blobs: %w", err)
	}
	return reserved, err
}

// clock is the store's time in unix milliseconds
func (s *Store) clock() int64 {
	return s.now().UnixMilli()
}

// step is a moment of a commit that the gates end the process at
type step int

const (
	stepWritten   step = iota // the upload's file holds every byte, not yet synced
	stepSynced                // the file synced in uploads/
	stepPublished             // renamed into objects/, its directory synced
	stepCommitted             // the row committed, the files it freed not yet removed
)

// reach runs the gates' hook at a step of a commit
func (s *Store) reach(at step) {
	if s.crashAt != nil {
		s.crashAt(at)
	}
}

// quietLog logs one kind of trouble once a quiet period, with how many times
// it happened since, so that a file a scanner holds for an hour is one line:
//
//	00:00  a removal refused     → Warn count=1
//	00:01…00:09 the same, 311×   → counted
//	00:10  once more             → Warn count=312
type quietLog struct {
	log      *slog.Logger
	message  string
	mu       sync.Mutex
	count    int
	loggedAt time.Time
}

func newQuietLog(log *slog.Logger, message string) *quietLog {
	return &quietLog{log: log, message: message}
}

func (q *quietLog) observe(now time.Time, last error) {
	q.mu.Lock()
	defer q.mu.Unlock()
	q.count++
	if !q.loggedAt.IsZero() && now.Sub(q.loggedAt) < quietFailures {
		return
	}
	q.log.Warn(q.message, "count", q.count, "last", last)
	q.count, q.loggedAt = 0, now
}
