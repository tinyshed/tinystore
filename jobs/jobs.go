package jobs

import (
	"context"
	"embed"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"sync"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/admission"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

//go:embed migrations/*.sql
var migrationFiles embed.FS

// the file this engine claims inside the store's directory
const fileName = "jobs.db"

// jobsApplicationID is "TJOB", the SQLite application id that claims a file for this engine
const jobsApplicationID = 0x544a4f42

var errClosed = fmt.Errorf("jobs: %w", tinystore.ErrClosed)

type Store struct {
	runtime     *tinystore.Store
	file        *sqlite.File
	log         *slog.Logger
	now         func() time.Time
	gate        admission.Gate
	writes      admission.Slots
	closing     chan struct{} // closed when Close begins, so that Work gives its jobs back
	maintenance chan struct{}
	ids         idRange
	closeOnce   sync.Once
	closeErr    error

	opened  sync.Mutex
	queues  map[string]*queueState
	repeats sync.Map // a kept repeat's text → Repeat, so that a zone is loaded once
}

// Open opens jobs.db inside the store and gives back the leases a process that
// died held, counting their attempts. The store closes it and, unless it is
// Manual, removes every minute what the queues keep no longer.
func Open(ctx context.Context, store *tinystore.Store, _ Options) (*Store, error) {
	path, release, err := store.Claim(fileName)
	if err != nil {
		return nil, err
	}

	s, err := openEngine(ctx, store, path)
	if err != nil {
		release()
		return nil, err
	}

	store.Every("jobs maintenance", maintainEvery, s.maintainInBackground)
	s.log.Info("opened", "path", path)
	return s, nil
}

// openEngine opens and migrates the file, ends the leases of the process that
// held it before, and hands the engine to the store
func openEngine(ctx context.Context, store *tinystore.Store, path string) (*Store, error) {
	file, err := openFile(ctx, path)
	if err != nil {
		return nil, err
	}
	s := &Store{
		runtime: store, file: file, log: store.Logger("jobs"), now: store.Now,
		writes: admission.NewSlots(writeSlots), closing: make(chan struct{}),
		maintenance: make(chan struct{}, 1), queues: map[string]*queueState{},
	}
	s.maintenance <- struct{}{}
	if err = s.endDeadLeases(ctx); err == nil {
		err = store.Attach(s)
	}
	if err != nil {
		return nil, errors.Join(err, file.Close())
	}
	return s, nil
}

func openFile(ctx context.Context, path string) (*sqlite.File, error) {
	file, err := sqlite.Open(ctx, path, sqlite.Config{Readers: readers, PageSize: pageSize})
	if err != nil {
		return nil, fmt.Errorf("jobs: open: %w", err)
	}
	scripts, err := fs.Sub(migrationFiles, "migrations")
	if err == nil {
		err = file.Migrate(ctx, jobsApplicationID, scripts)
	}
	if err != nil {
		return nil, errors.Join(fmt.Errorf("jobs: migrate: %w", err), file.Close())
	}
	return file, nil
}

// a lease the file still holds at open belongs to a process that died: its
// attempt counts, and its job is due again at once
const (
	countDeadAttempts = `update jobs set attempt = l.attempt from leases l
		where jobs.queue = l.queue and jobs.next = l.next and jobs.id = l.id`
	dropDeadLeases = `delete from leases`
)

func (s *Store) endDeadLeases(ctx context.Context) error {
	err := s.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		if _, err := w.ExecContext(ctx, countDeadAttempts); err != nil {
			return err
		}
		_, err := w.ExecContext(ctx, dropDeadLeases)
		return err
	})
	if err != nil {
		return fmt.Errorf("jobs: give back a dead process's leases: %w", err)
	}
	return nil
}

// Snapshot copies jobs.db into dir while the engine keeps working.
func (s *Store) Snapshot(ctx context.Context, dir string) (tinystore.SnapshotFile, error) {
	schema, err := s.file.Snapshot(ctx, tinystore.SnapshotPath(dir, fileName))
	if err != nil {
		return tinystore.SnapshotFile{}, fmt.Errorf("snapshot jobs: %w", err)
	}
	return tinystore.SnapshotFile{Name: fileName, Engine: "jobs", Schema: schema}, nil
}

// Close stops every Work, which gives back the jobs its handlers had without
// counting their attempts, waits for the work in flight and closes jobs.db;
// cancellation stops waiting, not the cleanup. The store calls it: an
// application closes the store instead.
func (s *Store) Close(ctx context.Context) error {
	s.closeOnce.Do(func() { close(s.closing) })
	drained, _ := s.gate.Close()
	select {
	case <-drained:
	case <-ctx.Done():
		return ctx.Err()
	}
	s.opened.Lock()
	defer s.opened.Unlock()
	if s.file != nil {
		s.closeErr = s.file.Close()
		s.file = nil
		s.log.Info("closed")
	}
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

// reserve holds a call's bytes in the store's memory before it materialises
// them; a store without Options.Memory is not asked
func (s *Store) reserve(ctx context.Context, bytes int) (release func(), err error) {
	if bytes <= 0 || s.runtime.Memory().Capacity == 0 {
		return func() {}, nil
	}
	reserved, err := s.runtime.Reserve(ctx, int64(bytes))
	if errors.Is(err, tinystore.ErrLimit) {
		return nil, fmt.Errorf("jobs: %w", err)
	}
	if err != nil {
		return nil, err
	}
	return reserved.Release, nil
}

// clock is the store's time in unix milliseconds, read once a call
func (s *Store) clock() int64 {
	return s.now().UnixMilli()
}

// idRange hands out job ids from a block reserved in meta, so that an id is
// never given twice, not after a crash either, for one write of meta a
// thousand jobs
type idRange struct {
	mu        sync.Mutex
	next, end int64
}

const reserveIDs = `update meta set value = value + ?1 where name = 'ids' returning value`

// nextID takes the next id of the store's block, reserving another in a
// transaction of its own when the block is spent; a Tx reserves its own
func (s *Store) nextID(ctx context.Context) (int64, error) {
	s.ids.mu.Lock()
	defer s.ids.mu.Unlock()
	if s.ids.next == s.ids.end {
		var end int64
		err := s.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
			return sqlite.QueryRow(ctx, w, reserveIDs, idBlock).Scan(&end)
		})
		if err != nil {
			return 0, fmt.Errorf("jobs: reserve ids: %w", err)
		}
		s.ids.next, s.ids.end = end-idBlock, end
	}
	s.ids.next++
	return s.ids.next, nil
}

// repeatOf is a kept repeat's parse, cached so that its zone loads once
func (s *Store) repeatOf(text string) (Repeat, error) {
	if cached, found := s.repeats.Load(text); found {
		if repeat, isRepeat := cached.(Repeat); isRepeat {
			return repeat, nil
		}
	}
	repeat, err := parseRepeat(text)
	if err == nil {
		s.repeats.Store(text, repeat)
	}
	return repeat, err
}
