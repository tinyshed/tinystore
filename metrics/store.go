package metrics

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
	"github.com/tinyshed/tinystore/codec"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

//go:embed migrations/*.sql
var migrationFiles embed.FS

type Store struct {
	runtime                                                 *tinystore.Store
	log                                                     *slog.Logger
	file                                                    *sqlite.File
	encoder, decoder                                        *codec.Codec
	metadata                                                *metadataCodec
	opts                                                    Options
	now                                                     func() time.Time
	mu                                                      sync.Mutex
	active                                                  int
	closing                                                 bool
	drained, closed                                         chan struct{}
	maintenanceGate                                         chan struct{}
	closeErr                                                error
	ingested, rejected, queried, sealed, expired, reclaimed atomic.Uint64
	readyCursor                                             atomic.Int64
	quarantined                                             atomic.Int64
	readSlots, ingestSlots                                  chan struct{}
	instruments                                             instruments
	flushing                                                sync.Mutex
	finalFlush                                              sync.Once
}

// the file this engine claims inside the store's directory
const fileName = "metrics.db"

// Open opens metrics.db inside the store. The store closes it, and unless it
// is Manual runs Maintain every Options.MaintenanceInterval, a minute by default.
func Open(ctx context.Context, runtime *tinystore.Store, options Options) (*Store, error) {
	opts, err := normalizeOptions(options)
	if err != nil {
		return nil, err
	}

	path, release, err := runtime.Claim(fileName)
	if err != nil {
		return nil, err
	}

	store, err := openEngine(ctx, runtime, path, opts)
	if err != nil {
		release()
		return nil, err
	}

	runtime.Every("metrics maintenance", opts.MaintenanceInterval, store.maintainInBackground)
	runtime.Every("metrics instruments", opts.Flush, store.Flush)
	store.log.Info("opened", "path", path, "suspended", store.quarantined.Load())
	return store, nil
}

// openEngine opens and migrates the file, then hands the engine to the store
func openEngine(ctx context.Context, runtime *tinystore.Store, path string, opts Options) (*Store, error) {
	f, err := openFile(ctx, path, opts.MaxReaders)
	if err != nil {
		return nil, err
	}
	store, err := newStore(ctx, f, opts)
	if err != nil {
		return nil, errors.Join(err, f.Close())
	}
	store.runtime, store.log, store.now = runtime, runtime.Logger("metrics"), runtime.Now
	if err = runtime.Attach(store); err != nil {
		return nil, errors.Join(err, store.Close(ctx))
	}
	return store, nil
}

// Snapshot copies metrics.db into dir while the engine keeps working.
func (s *Store) Snapshot(ctx context.Context, dir string) (tinystore.SnapshotFile, error) {
	schema, err := s.file.Snapshot(ctx, tinystore.SnapshotPath(dir, fileName))
	if err != nil {
		return tinystore.SnapshotFile{}, fmt.Errorf("snapshot metrics: %w", err)
	}
	return tinystore.SnapshotFile{Name: fileName, Engine: "metrics", Schema: schema}, nil
}

func (s *Store) maintainInBackground(ctx context.Context) error {
	_, err := s.Maintain(ctx)
	return err
}

// metricsApplicationID is "TMET", the SQLite application id that claims a file for this engine
const metricsApplicationID = 0x544d4554

func openFile(ctx context.Context, path string, readers int) (*sqlite.File, error) {
	f, err := sqlite.Open(ctx, path, readers)
	if err != nil {
		return nil, fmt.Errorf("open metrics file: %w", err)
	}
	scripts, err := fs.Sub(migrationFiles, "migrations")
	if err != nil {
		return nil, errors.Join(fmt.Errorf("open metrics migrations: %w", err), f.Close())
	}
	if err = f.Migrate(ctx, metricsApplicationID, scripts); err != nil {
		return nil, errors.Join(fmt.Errorf("migrate metrics: %w", err), f.Close())
	}
	return f, nil
}

// newStore builds the handle over an open file; the caller closes the file on an error
func newStore(ctx context.Context, f *sqlite.File, opts Options) (*Store, error) {
	quarantined, err := countSuspended(ctx, f)
	if err != nil {
		return nil, err
	}
	encoder, err := codec.New()
	if err != nil {
		return nil, fmt.Errorf("create metrics encoder: %w", err)
	}
	decoder, err := codec.New()
	if err != nil {
		return nil, errors.Join(fmt.Errorf("create metrics decoder: %w", err), encoder.Close())
	}
	metadata, err := newMetadataCodec()
	if err != nil {
		return nil, errors.Join(err, encoder.Close(), decoder.Close())
	}
	store := &Store{
		file: f, encoder: encoder, decoder: decoder, metadata: metadata, opts: opts, now: time.Now,
		log:     slog.New(slog.DiscardHandler),
		drained: make(chan struct{}), closed: make(chan struct{}), maintenanceGate: make(chan struct{}, 1),
		readSlots:   make(chan struct{}, opts.MaxConcurrentReads),
		ingestSlots: make(chan struct{}, opts.MaxConcurrentIngest),
	}
	store.instruments.store = store
	store.maintenanceGate <- struct{}{}
	store.quarantined.Store(quarantined)
	return store, nil
}

const countSuspendedQuery = `select count(*) from series_state where failed_at is not null`

func countSuspended(ctx context.Context, f *sqlite.File) (int64, error) {
	var suspended int64
	err := f.View(ctx, func(tx *sql.Tx) error {
		return tx.QueryRowContext(ctx, countSuspendedQuery).Scan(&suspended)
	})
	if err != nil {
		return 0, fmt.Errorf("count suspended maintenance: %w", err)
	}
	return suspended, nil
}

// Close ingests the instruments' last values, stops admission and drains
// in-flight work; cancellation stops waiting, not cleanup. The store calls it:
// an application closes the store instead.
func (s *Store) Close(ctx context.Context) error {
	var flushErr error
	s.finalFlush.Do(func() { flushErr = s.Flush(ctx) })

	s.mu.Lock()
	if !s.closing {
		s.closing = true
		if s.active == 0 {
			close(s.drained)
		}
		go func() {
			<-s.drained
			s.closeErr = errors.Join(s.encoder.Close(), s.decoder.Close(), s.metadata.close(), s.file.Close())
			s.log.Info("closed")
			close(s.closed)
		}()
	}
	s.mu.Unlock()
	select {
	case <-s.closed:
		return errors.Join(flushErr, s.closeErr)
	case <-ctx.Done():
		return errors.Join(flushErr, ctx.Err())
	}
}

func (s *Store) Stats() Stats {
	return Stats{
		IngestedSamples:   s.ingested.Load(),
		RejectedBatches:   s.rejected.Load(),
		Queries:           s.queried.Load(),
		SealedBlocks:      s.sealed.Load(),
		ExpiredSamples:    s.expired.Load(),
		QuarantinedSeries: uint64(s.quarantined.Load()), //nolint:gosec // a count of rows, never negative
		ReclaimedSeries:   s.reclaimed.Load(),
	}
}
