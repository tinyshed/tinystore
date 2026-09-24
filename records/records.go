package records

import (
	"context"
	"database/sql"
	"embed"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

//go:embed migrations/*.sql
var migrationFiles embed.FS

// the file this engine claims inside the store's directory
const fileName = "records.db"

// recordsApplicationID is "TREC", the SQLite application id that claims a file for this engine
const recordsApplicationID = 0x54524543

type Options struct {
	// Buffer is how many records may wait in memory; beyond it a record is
	// dropped and counted, never waited for. 1024 when zero.
	Buffer int

	// Flush is how often waiting records are written. One second when zero.
	Flush time.Duration

	// Retention is how long a record is kept. Fourteen days when zero.
	Retention time.Duration
}

// Record is one log line as it was handled: attributes in groups are named
// group.key, and values come back as JSON decodes them.
type Record struct {
	At      time.Time
	Level   slog.Level
	Message string
	Attrs   map[string]any
}

type Stats struct {
	Written, Dropped, Expired uint64
}

type Store struct {
	file    *sqlite.File
	log     *slog.Logger
	now     func() time.Time
	opts    Options
	queue   chan Record
	written atomic.Uint64
	dropped atomic.Uint64
	expired atomic.Uint64
}

// Open opens records.db inside the store. The store closes it and, unless it
// is Manual, writes waiting records every Options.Flush and removes expired
// ones every hour.
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
	store.Every("records retention", time.Hour, engine.Maintain)
	engine.log.Info("opened", "path", path)
	return engine, nil
}

func normalizeOptions(o Options) (Options, error) {
	if o.Buffer < 0 || o.Flush < 0 || o.Retention < 0 {
		return o, fmt.Errorf("%w: records options", tinystore.ErrInvalid)
	}
	if o.Buffer == 0 {
		o.Buffer = 1024
	}
	if o.Flush == 0 {
		o.Flush = time.Second
	}
	if o.Retention == 0 {
		o.Retention = 14 * 24 * time.Hour
	}
	return o, nil
}

func openEngine(ctx context.Context, store *tinystore.Store, path string, opts Options) (*Store, error) {
	file, err := sqlite.Open(ctx, path, 2)
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

	engine := &Store{
		file: file, log: store.Logger("records"), now: store.Now, opts: opts,
		queue: make(chan Record, opts.Buffer),
	}
	if err = store.Attach(engine); err != nil {
		return nil, errors.Join(err, file.Close())
	}
	return engine, nil
}

const insertRecord = `insert into records (at, level, message, attrs) values (?, ?, ?, ?)`

// Flush writes every waiting record in one transaction.
func (s *Store) Flush(ctx context.Context) error {
	batch := s.drain()
	if len(batch) == 0 {
		return nil
	}

	err := s.file.Update(ctx, func(tx *sql.Tx) error {
		for _, record := range batch {
			if err := writeRecord(ctx, tx, record); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		s.dropped.Add(uint64(len(batch)))
		return err
	}
	s.written.Add(uint64(len(batch)))
	return nil
}

func (s *Store) drain() []Record {
	var batch []Record
	for {
		select {
		case record := <-s.queue:
			batch = append(batch, record)
		default:
			return batch
		}
	}
}

func writeRecord(ctx context.Context, tx *sql.Tx, record Record) error {
	attrs, err := json.Marshal(record.Attrs)
	if err != nil {
		return fmt.Errorf("records: encode attributes of %q: %w", record.Message, err)
	}
	_, err = tx.ExecContext(ctx, insertRecord, record.At.UnixMilli(), int(record.Level), record.Message, string(attrs))
	if err != nil {
		return fmt.Errorf("records: write: %w", err)
	}
	return nil
}

const expireRecords = `delete from records where at < ?`

// Maintain removes the records older than Options.Retention by the store's clock.
func (s *Store) Maintain(ctx context.Context) error {
	cutoff := s.now().Add(-s.opts.Retention).UnixMilli()
	return s.file.Update(ctx, func(tx *sql.Tx) error {
		result, err := tx.ExecContext(ctx, expireRecords, cutoff)
		if err != nil {
			return fmt.Errorf("records: expire: %w", err)
		}
		removed, err := result.RowsAffected()
		if err == nil && removed > 0 {
			s.expired.Add(uint64(removed))
		}
		return err
	})
}

// Snapshot copies records.db into dir while the engine keeps working; what
// waits in the buffer is not in the copy.
func (s *Store) Snapshot(ctx context.Context, dir string) (tinystore.SnapshotFile, error) {
	schema, err := s.file.Snapshot(ctx, tinystore.SnapshotPath(dir, fileName))
	if err != nil {
		return tinystore.SnapshotFile{}, fmt.Errorf("snapshot records: %w", err)
	}
	return tinystore.SnapshotFile{Name: fileName, Engine: "records", Schema: schema}, nil
}

func (s *Store) Stats() Stats {
	return Stats{Written: s.written.Load(), Dropped: s.dropped.Load(), Expired: s.expired.Load()}
}

// Close writes what is still waiting, then closes records.db. The store calls
// it: an application closes the store instead.
func (s *Store) Close(ctx context.Context) error {
	err := errors.Join(s.Flush(ctx), s.file.Close())
	s.log.Info("closed", "written", s.written.Load(), "dropped", s.dropped.Load())
	return err
}
