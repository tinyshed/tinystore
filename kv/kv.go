package kv

import (
	"context"
	"database/sql"
	"embed"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/admission"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

//go:embed migrations/*.sql
var migrationFiles embed.FS

const fileName = "kv.db"

// kvApplicationID is "TKVS", the SQLite application id that claims a file for this engine
const kvApplicationID = 0x544b5653

var errClosed = fmt.Errorf("kv: %w", tinystore.ErrClosed)

type Store struct {
	runtime     *tinystore.Store
	file        *sqlite.File
	hosted      bool // the file is a database's, In which the buckets live
	log         *slog.Logger
	now         func() time.Time
	gate        admission.Gate
	writes      admission.Slots
	revision    atomic.Int64
	maintenance chan struct{}
	closing     sync.Once
	closeErr    error
	clearBound  int // keys a Clear deletes in its transaction before it marks instead
	expireBound int // expired keys a transaction of maintenance deletes

	backlog atomic.Bool // the last Maintain stopped at a bound with rows left to delete

	opened   sync.Mutex
	counters map[string]openCounters
	configs  map[string]*configHub
	renewals renewals
	runs     onceRuns
	leave    func() // gate.Leave, bound once rather than at every call
}

// openCounters is how the counters of one name are open in this process:
// every handle on them keeps its numbers the same way, in one memory or none
type openCounters struct {
	loseAtMost time.Duration
	memory     *memory
}

// Open opens kv.db inside the store, or with Options.In a database's file.
// The store closes it and, unless it is Manual, deletes expired keys every
// minute, and every ten seconds while the last pass stopped at its bound with
// expired or cleared rows left.
func Open(ctx context.Context, store *tinystore.Store, options Options) (*Store, error) {
	if options.In != nil {
		return openIn(ctx, store, options.In)
	}
	path, release, err := store.Claim(fileName)
	if err != nil {
		return nil, err
	}

	file, err := openFile(ctx, path, store.Readers(readers))
	if err != nil {
		release()
		return nil, err
	}
	state, err := openEngine(ctx, store, file, false)
	if err != nil {
		release()
		return nil, errors.Join(err, file.Close())
	}
	state.log.Info("opened", "path", path)
	return state, nil
}

// hosts are the databases' files a store keeps its buckets in, one store
// each, so that two stores never take versions of one file's revision
var hosts sync.Map // *sqlite.File → *Store

// openIn keeps the buckets in a database's file, beside its rows: the tables
// migrate with a history of their own, and the database keeps the file,
// which it closes after this store, having opened before it.
func openIn(ctx context.Context, store *tinystore.Store, db Database) (*Store, error) {
	if store.Guest() {
		return nil, fmt.Errorf("%w: kv opens only in the store that holds %s, which keeps its state",
			tinystore.ErrInvalid, store.Dir())
	}
	file := db.SQLiteFile()
	if file == nil {
		return nil, fmt.Errorf("%w: kv: In a database that is closed", tinystore.ErrInvalid)
	}
	if _, taken := hosts.LoadOrStore(file, (*Store)(nil)); taken {
		return nil, fmt.Errorf("%w: kv: the database holds another kv store", tinystore.ErrInUse)
	}
	scripts, err := fs.Sub(migrationFiles, "migrations")
	if err == nil {
		err = file.MigrateHosted(ctx, "kv", scripts)
	}
	var state *Store
	if err == nil {
		state, err = openEngine(ctx, store, file, true)
	}
	if err != nil {
		hosts.Delete(file)
		return nil, fmt.Errorf("kv: open in a database: %w", err)
	}
	hosts.Store(file, state)
	state.log.Info("opened in a database")
	return state, nil
}

// openEngine reads the file's revision, hands the engine to the store and has
// it maintained
func openEngine(ctx context.Context, store *tinystore.Store, file *sqlite.File, hosted bool) (*Store, error) {
	state := &Store{
		runtime: store, file: file, hosted: hosted, log: store.Logger("kv"), now: store.Now,
		writes: admission.NewSlots(writeSlots), maintenance: make(chan struct{}, 1),
		clearBound: clearAtOnce, expireBound: expiryBatch, counters: map[string]openCounters{},
		configs:  map[string]*configHub{},
		renewals: renewals{waiting: map[renewed]renewal{}},
	}
	state.maintenance <- struct{}{}
	state.leave = state.gate.Leave
	err := state.loadRevision(ctx)
	if err == nil {
		err = store.Attach(state)
	}
	if err != nil {
		return nil, err
	}
	store.EveryEngine("kv", "kv expiry", expiryEvery, state.maintainInBackground)
	store.EveryEngine("kv", "kv expiry backlog", catchUpEvery, state.catchUpInBackground)
	return state, nil
}

func openFile(ctx context.Context, path string, readers int) (*sqlite.File, error) {
	file, err := sqlite.Open(ctx, path, sqlite.Config{Readers: readers, PageSize: pageSize, WriterCache: writerCache})
	if err != nil {
		return nil, fmt.Errorf("kv: open: %w", err)
	}
	scripts, err := fs.Sub(migrationFiles, "migrations")
	if err == nil {
		err = file.Migrate(ctx, kvApplicationID, scripts)
	}
	if err != nil {
		return nil, errors.Join(fmt.Errorf("kv: migrate: %w", err), file.Close())
	}
	return file, nil
}

const (
	selectRevision = `select value from _tinystore_kv_meta as meta where name = 'revision'`
	updateRevision = `update _tinystore_kv_meta as meta set value = ?1 where name = 'revision'`
)

// loadRevision takes up the file's high-water mark, so that no version repeats
// one an earlier process wrote
func (s *Store) loadRevision(ctx context.Context) error {
	var revision int64
	err := s.file.View(ctx, func(tx *sql.Tx) error {
		return tx.QueryRowContext(ctx, selectRevision).Scan(&revision)
	})
	if err != nil {
		return fmt.Errorf("kv: read the revision: %w", err)
	}
	s.revision.Store(revision)
	return nil
}

// nextRevision is the version of one write, kept as the file's high-water mark
// in the write's own transaction. A write rolled back leaves a gap, never a
// repeat.
func (s *Store) nextRevision(ctx context.Context, w sqlite.Writer) (int64, error) {
	revision := s.revision.Add(1)
	_, err := w.ExecContext(ctx, updateRevision, revision)
	return revision, err
}

// Snapshot copies kv.db into dir while the engine keeps working; buckets kept
// In a database are in that database's copy.
func (s *Store) Snapshot(ctx context.Context, dir string) ([]tinystore.SnapshotFile, error) {
	if s.hosted {
		return nil, nil
	}
	schema, err := s.file.Snapshot(ctx, tinystore.SnapshotPath(dir, fileName))
	if err != nil {
		return nil, fmt.Errorf("snapshot kv: %w", err)
	}
	return []tinystore.SnapshotFile{{Name: fileName, Engine: "kv", Schema: schema}}, nil
}

// Close lets the work in flight finish, writes what LoseAtMost counters hold
// and the renewals reads asked for, and closes kv.db, or leaves a database's
// file to it; cancellation stops waiting, not the cleanup. The store calls it: an application closes the
// store instead.
func (s *Store) Close(ctx context.Context) error {
	drained, _ := s.gate.Close()
	select {
	case <-drained:
	case <-ctx.Done():
		return ctx.Err()
	}
	s.closing.Do(func() {
		_, countersErr := s.flushCounters(ctx)
		_, renewalsErr := s.flushRenewals(ctx)
		var closeErr error
		if s.hosted {
			hosts.Delete(s.file)
		} else {
			closeErr = s.file.Close()
		}
		s.closeErr = errors.Join(countersErr, renewalsErr, closeErr)
		s.log.Info("closed")
	})
	return s.closeErr
}

// memoryFor is the memory of the counters name, the one their first opening
// made.
//
// Opening them again with another LoseAtMost, or without, is ErrInvalid, since
// one handle would read the file while another holds newer numbers.
func (s *Store) memoryFor(name string, bucket int64, loseAtMost time.Duration) (*memory, error) {
	s.opened.Lock()
	defer s.opened.Unlock()
	if open, ok := s.counters[name]; ok {
		if open.loseAtMost != loseAtMost {
			return nil, fmt.Errorf("%w: kv: counters %q are open %s and opened again %s",
				tinystore.ErrInvalid, name, keptBy(open.loseAtMost), keptBy(loseAtMost))
		}
		return open.memory, nil
	}
	var held *memory
	if loseAtMost > 0 {
		held = newMemory(s, bucket, name)
		s.runtime.EveryEngine("kv", "kv flush "+name, loseAtMost, held.flushInBackground)
	}
	s.counters[name] = openCounters{loseAtMost: loseAtMost, memory: held}
	return held, nil
}

func keptBy(loseAtMost time.Duration) string {
	if loseAtMost == 0 {
		return "without LoseAtMost"
	}
	return fmt.Sprintf("with LoseAtMost(%v)", loseAtMost)
}

// flushCounters writes what every LoseAtMost memory holds, and returns how
// many counters it wrote
func (s *Store) flushCounters(ctx context.Context) (int, error) {
	s.opened.Lock()
	var memories []*memory
	for _, open := range s.counters {
		if open.memory != nil {
			memories = append(memories, open.memory)
		}
	}
	s.opened.Unlock()

	written := 0
	var errs []error
	for _, held := range memories {
		wrote, err := held.flush(ctx)
		written += wrote
		if err != nil {
			errs = append(errs, fmt.Errorf("counter bucket %q: %w", held.name, err))
		}
	}
	return written, errors.Join(errs...)
}

func (s *Store) admit(ctx context.Context) (release func(), err error) {
	if err = s.gate.Enter(ctx, errClosed); err != nil {
		return nil, err
	}
	return s.leave, nil
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

// reserve holds an operation's weight in the store's memory, waiting in
// arrival order until it fits
func (s *Store) reserve(ctx context.Context, bytes int) (*tinystore.Reservation, error) {
	reserved, err := s.runtime.Reserve(ctx, int64(max(bytes, 1)))
	if errors.Is(err, tinystore.ErrLimit) {
		return nil, fmt.Errorf("kv: %w", err)
	}
	return reserved, err
}

// reserveNow holds an operation's weight in the store's memory if it is free
// at once
func (s *Store) reserveNow(bytes int) (*tinystore.Reservation, error) {
	reserved, err := s.runtime.ReserveNow(int64(max(bytes, 1)))
	if err != nil {
		return nil, fmt.Errorf("kv: inside Tx or View, which hold what the calls holding memory wait for: %w", err)
	}
	return reserved, nil
}
