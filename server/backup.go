package server

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/backup"
	"github.com/tinyshed/tinystore/server/wire"
	"github.com/tinyshed/tinystore/sqldb"
)

// serverBackup is a download of the whole store as one zip, as backup.Write
// makes it, for an admin's connection alone. An engine whose file is in the
// directory is opened first, so that the snapshot holds it, and a database no
// client opened is copied without opening it.
func serverBackup(c *call) error {
	var ask wire.Backup
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	var options []backup.Option
	for _, name := range ask.Files {
		options = append(options, backup.File(name))
	}
	if c.session.capability != wire.Admin {
		return fmt.Errorf("%w: server: a backup", errAdminOnly)
	}
	s := c.session.server
	if err := s.openEnginesOnDisk(c.ctx); err != nil {
		return err
	}
	snapshot, err := s.snapshotAll(c.ctx)
	if err != nil {
		return err
	}
	defer func() {
		if removeErr := snapshot.Remove(); removeErr != nil {
			s.log.Warn("a backup's snapshot is left in the directory", "dir", snapshot.Dir, "err", removeErr)
		}
	}()

	if err = begin(c, wire.Empty{}); err != nil {
		return err
	}
	out := &chunks{call: c, buffer: make([]byte, 0, min(transferChunk, int(c.session.agreed.maxBody)))}
	if err = backup.WriteSnapshot(c.ctx, snapshot, s.store.Now(), out, options...); err != nil {
		return err
	}
	return out.end()
}

// enginesOnDisk are the engines whose files a store's directory may hold, by
// the name each claims
var enginesOnDisk = []struct {
	name string
	open func(s *Server, ctx context.Context) error
}{
	{"kv.db", func(s *Server, ctx context.Context) error { _, err := s.kvStore(ctx); return err }},
	{"jobs.db", func(s *Server, ctx context.Context) error { _, err := s.jobsStore(ctx); return err }},
	{"blobs", func(s *Server, ctx context.Context) error { _, err := s.blobsStore(ctx); return err }},
	{"records.db", func(s *Server, ctx context.Context) error { _, err := s.recordsStore(ctx); return err }},
	{"metrics.db", func(s *Server, ctx context.Context) error { _, err := s.metricsStore(ctx); return err }},
}

// openEnginesOnDisk opens each engine whose file the directory holds and no
// client has asked for yet, as a client's first call would, and makes none
// that is not there
func (s *Server) openEnginesOnDisk(ctx context.Context) error {
	for _, engine := range enginesOnDisk {
		if _, err := os.Stat(filepath.Join(s.store.Dir(), engine.name)); errors.Is(err, os.ErrNotExist) {
			continue
		}
		if err := engine.open(s, ctx); err != nil {
			return err
		}
	}
	return nil
}

// snapshotAll is the store's snapshot of its open engines, and a copy of each
// database no client opened. A client's open of a database waits for it.
func (s *Server) snapshotAll(ctx context.Context) (tinystore.Snapshot, error) {
	s.sqlOpening.Lock()
	defer s.sqlOpening.Unlock()
	snapshot, err := s.store.Snapshot(ctx)
	if err != nil {
		return snapshot, err
	}
	paths, err := filepath.Glob(filepath.Join(s.store.Dir(), "sql", "*.db"))
	if err != nil {
		return snapshot, errors.Join(err, snapshot.Remove())
	}
	for _, path := range paths {
		name := strings.TrimSuffix(filepath.Base(path), ".db")
		if s.databases[name] != nil {
			continue
		}
		copied, copyErr := sqldb.Copy(ctx, s.store, name, snapshot.Dir)
		switch {
		case errors.Is(copyErr, tinystore.ErrInUse):
			// open in the program serving its own store, which the snapshot holds
		case copyErr != nil:
			return snapshot, errors.Join(copyErr, snapshot.Remove())
		default:
			snapshot.Files = append(snapshot.Files, copied)
		}
	}
	return snapshot, nil
}

// chunks is an io.Writer of a download's DATA, each within the body the
// client agreed to take
type chunks struct {
	call   *call
	buffer []byte
}

func (w *chunks) Write(p []byte) (int, error) {
	written := 0
	for len(p) > 0 {
		n := copy(w.buffer[len(w.buffer):cap(w.buffer)], p)
		w.buffer, p, written = w.buffer[:len(w.buffer)+n], p[n:], written+n
		if len(w.buffer) == cap(w.buffer) {
			if err := w.call.chunk(w.buffer, false); err != nil {
				return written, err
			}
			w.buffer = w.buffer[:0]
		}
	}
	return written, nil
}

// end sends what the buffer holds as the last DATA, with END
func (w *chunks) end() error {
	return w.call.chunk(w.buffer, true)
}
