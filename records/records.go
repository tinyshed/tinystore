package records

import (
	"context"
	"embed"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"sync"
	"sync/atomic"
	"time"

	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

//go:embed migrations/*.sql
var migrationFiles embed.FS

// the file this engine claims inside the store's directory
const fileName = "records.db"

// recordsApplicationID is "TREC", the SQLite application id that claims a file for this engine
const recordsApplicationID = 0x54524543

// pageSize is chosen once, when the file is created: a block's remainder is
// left on a leaf page, and 7 KB blocks left a tenth of a 4 KiB-page table empty
const pageSize = 1024

type Store struct {
	runtime     *tinystore.Store
	file        *sqlite.File
	log         *slog.Logger
	now         func() time.Time
	opts        Options
	blobs       *zstd.Encoder
	unpack      *zstd.Decoder
	streams     streams
	queue       chan Record
	gate        gate
	maintenance chan struct{}
	finalFlush  sync.Once
	closing     sync.Once
	closeErr    error

	appended, dropped, sealed, expired, queries atomic.Uint64
	readBlocks, readBytes                       atomic.Uint64
}

// Open opens records.db inside the store. The store closes it and, unless it
// is Manual, writes what the handler holds every Options.Flush, and seals and
// expires every minute.
func Open(ctx context.Context, store *tinystore.Store, options Options) (*Store, error) {
	opts, err := normalizeOptions(options)
	if err != nil {
		return nil, err
	}

	path, release, err := store.Claim(fileName)
	if err != nil {
		return nil, err
	}

	engine, err := openEngine(ctx, store, path, opts)
	if err != nil {
		release()
		return nil, err
	}

	store.Every("records flush", opts.Flush, engine.Flush)
	store.Every("records maintenance", maintenanceEvery, engine.maintainInBackground)
	engine.log.Info("opened", "path", path)
	return engine, nil
}

// openEngine opens and migrates the file, then hands the engine to the store
func openEngine(ctx context.Context, store *tinystore.Store, path string, opts Options) (*Store, error) {
	file, err := openFile(ctx, path)
	if err != nil {
		return nil, err
	}
	engine, err := newStore(ctx, file, opts)
	if err != nil {
		return nil, errors.Join(err, file.Close())
	}
	engine.runtime, engine.log, engine.now = store, store.Logger("records"), store.Now
	if err = store.Attach(engine); err != nil {
		return nil, errors.Join(err, engine.release())
	}
	return engine, nil
}

func openFile(ctx context.Context, path string) (*sqlite.File, error) {
	file, err := sqlite.Open(ctx, path, sqlite.Config{Readers: 2, PageSize: pageSize})
	if err != nil {
		return nil, fmt.Errorf("records: open: %w", err)
	}
	scripts, err := fs.Sub(migrationFiles, "migrations")
	if err == nil {
		err = file.Migrate(ctx, recordsApplicationID, scripts)
	}
	if err != nil {
		return nil, errors.Join(fmt.Errorf("records: migrate: %w", err), file.Close())
	}
	return file, nil
}

// newStore builds the handle over an open file; the caller closes the file on an error
func newStore(ctx context.Context, file *sqlite.File, opts Options) (*Store, error) {
	blobs, unpack, err := newBlobCoders()
	if err != nil {
		return nil, err
	}
	engine := &Store{
		file: file, opts: opts, blobs: blobs, unpack: unpack, now: time.Now,
		log:   slog.New(slog.DiscardHandler),
		queue: make(chan Record, opts.Buffer), maintenance: make(chan struct{}, 1),
	}
	engine.gate.drained = make(chan struct{})
	engine.maintenance <- struct{}{}
	if err = engine.streams.load(ctx, file); err != nil {
		_ = blobs.Close()
		unpack.Close()
		return nil, err
	}
	return engine, nil
}

func (s *Store) maintainInBackground(ctx context.Context) error {
	_, err := s.Maintain(ctx)
	return err
}

// Snapshot copies records.db into dir while the engine keeps working; what the
// handler holds in memory is not in the copy, and the head is.
func (s *Store) Snapshot(ctx context.Context, dir string) (tinystore.SnapshotFile, error) {
	schema, err := s.file.Snapshot(ctx, tinystore.SnapshotPath(dir, fileName))
	if err != nil {
		return tinystore.SnapshotFile{}, fmt.Errorf("snapshot records: %w", err)
	}
	return tinystore.SnapshotFile{Name: fileName, Engine: "records", Schema: schema}, nil
}

func (s *Store) Stats() Stats {
	return Stats{
		Appended: s.appended.Load(), Dropped: s.dropped.Load(),
		SealedSegments: s.sealed.Load(), ExpiredSegments: s.expired.Load(), Queries: s.queries.Load(),
		ReadBlocks: s.readBlocks.Load(), ReadBytes: s.readBytes.Load(),
	}
}

// Close writes what the handler still holds, lets the work in flight finish
// and closes records.db; cancellation stops waiting, not the cleanup. The
// store calls it: an application closes the store instead.
func (s *Store) Close(ctx context.Context) error {
	var flushErr error
	s.finalFlush.Do(func() { flushErr = s.Flush(ctx) })

	drained := s.gate.close()
	select {
	case <-drained:
	case <-ctx.Done():
		return errors.Join(flushErr, ctx.Err())
	}

	s.closing.Do(func() {
		s.closeErr = s.release()
		s.log.Info("closed", "appended", s.appended.Load(), "dropped", s.dropped.Load())
	})
	return errors.Join(flushErr, s.closeErr)
}

// release closes what newStore opened, the file last
func (s *Store) release() error {
	s.unpack.Close()
	return errors.Join(s.blobs.Close(), s.file.Close())
}
