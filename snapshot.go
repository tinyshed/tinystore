package tinystore

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
)

// Snapshotter is an engine that can copy its file while it keeps working.
type Snapshotter interface {
	Snapshot(ctx context.Context, dir string) (SnapshotFile, error)
}

// SnapshotFile is one engine's copy: Name is its path inside the store, such
// as "metrics.db" or "sql/app.db", and Schema how many migrations it has.
type SnapshotFile struct {
	Name   string `json:"name"`
	Engine string `json:"engine"`
	Schema int    `json:"schema"`
}

// Snapshot is a copy of every engine's file, in Dir inside the store's
// directory, so that it takes the space of the disk the data is on.
type Snapshot struct {
	Dir   string
	Files []SnapshotFile
}

// Remove deletes the copies.
func (s Snapshot) Remove() error {
	return os.RemoveAll(s.Dir)
}

// Snapshot copies every attached engine's file. Each copy is one moment of its
// engine; two engines are two moments, as no write spans two engines anyway.
// The engines keep working, and Close waits for the copying to finish.
func (s *Store) Snapshot(ctx context.Context) (Snapshot, error) {
	engines, err := s.holdEngines()
	if err != nil {
		return Snapshot{}, err
	}
	defer s.running.Done()

	dir, err := os.MkdirTemp(s.dir, ".snapshot-")
	if err != nil {
		return Snapshot{}, fmt.Errorf("snapshot: %w", err)
	}
	snapshot := Snapshot{Dir: dir}
	for _, engine := range engines {
		snapshotter, ok := engine.(Snapshotter)
		if !ok {
			continue
		}
		file, err := snapshotter.Snapshot(ctx, dir)
		if err != nil {
			return Snapshot{}, errors.Join(err, snapshot.Remove())
		}
		snapshot.Files = append(snapshot.Files, file)
	}
	return snapshot, nil
}

// holdEngines counts a snapshot as running work, so that Close waits for it
// before it closes the files being copied
func (s *Store) holdEngines() ([]Engine, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return nil, ErrClosed
	}
	s.running.Add(1)
	return append([]Engine(nil), s.engines...), nil
}

// SnapshotPath is where an engine writes its copy of name inside dir.
func SnapshotPath(dir, name string) string {
	return filepath.Join(dir, filepath.FromSlash(name))
}
