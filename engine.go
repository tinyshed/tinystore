package tinystore

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"path"
	"path/filepath"
	"strings"
	"time"

	"github.com/tinyshed/tinystore/internal/dirlock"
)

type Engine interface {
	Close(ctx context.Context) error
}

// Claim reserves a file or directory inside the store for one engine and
// returns its path, creating the directory it lies in, or the directory
// itself for a name that ends in /. release gives the name back to an engine
// that failed to open; an attached engine keeps it until the store closes.
//
//	metrics.db  records.db  blobs/    engines
//	sql/<name>.db                     databases the application names
func (s *Store) Claim(name string) (filePath string, release func(), err error) {
	directory := strings.HasSuffix(name, "/")
	name = path.Clean(name)
	if name == "." || name == dirlock.Name || !filepath.IsLocal(filepath.FromSlash(name)) {
		return "", nil, fmt.Errorf("%w: %q is not a name inside the store", ErrInvalid, name)
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return "", nil, ErrClosed
	}
	if s.claimed[name] {
		return "", nil, fmt.Errorf("%w: %s", ErrInUse, name)
	}

	filePath = filepath.Join(s.dir, filepath.FromSlash(name))
	made := filepath.Dir(filePath)
	if directory {
		made = filePath
	}
	if err := os.MkdirAll(made, 0o750); err != nil {
		return "", nil, fmt.Errorf("create %s: %w", made, err)
	}
	s.claimed[name] = true
	return filePath, func() { s.unclaim(name) }, nil
}

func (s *Store) unclaim(name string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.claimed, name)
}

// Attach hands an opened engine to the store, which closes it on Close.
func (s *Store) Attach(engine Engine) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return ErrClosed
	}
	if writer, ok := engine.(SelfWriter); ok && s.self != nil {
		if s.self.writer != nil {
			return fmt.Errorf("%w: self-metrics writer", ErrInUse)
		}
		s.self.writer = writer
	}
	s.engines = append(s.engines, engine)
	return nil
}

// Logger is the application's logger, labelled with the engine that logs.
func (s *Store) Logger(engine string) *slog.Logger {
	return s.logger.With("engine", engine)
}

func (s *Store) Now() time.Time {
	return s.clock()
}
