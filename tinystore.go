package tinystore

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"slices"
	"sync"
	"time"
)

type Options struct {
	// Logger receives TinyStore's own logs; nil discards them.
	Logger *slog.Logger

	// Manual runs no background work: the application calls each engine's
	// maintenance itself. Tests usually want this.
	Manual bool

	// Clock replaces time.Now for the store and every engine opened against it.
	Clock func() time.Time

	// Memory bounds the bytes that all engines' in-flight work holds at once;
	// zero leaves each engine to its own per-call limits.
	Memory int64
}

type Store struct {
	dir    string
	logger *slog.Logger
	clock  func() time.Time
	manual bool
	lock   io.Closer
	memory *memory

	background context.Context
	stop       context.CancelFunc
	running    sync.WaitGroup

	mu      sync.Mutex
	closed  bool
	claimed map[string]bool
	engines []Engine

	closing  sync.Once
	done     chan struct{}
	closeErr error
}

// Open creates dir if needed and holds it until Close; a second store on the
// same directory, in this process or another, is refused with ErrInUse. ctx
// bounds the opening only: background work lives until Close.
func Open(ctx context.Context, dir string, options Options) (*Store, error) {
	if dir == "" || options.Memory < 0 {
		return nil, fmt.Errorf("%w: empty directory or negative memory", ErrInvalid)
	}
	if err := os.MkdirAll(dir, 0o750); err != nil {
		return nil, fmt.Errorf("create %s: %w", dir, err)
	}

	lock, err := lockDirectory(dir)
	if err != nil {
		return nil, err
	}

	store := newStore(ctx, dir, options, lock)
	store.logOpened()
	return store, nil
}

func newStore(ctx context.Context, dir string, options Options, lock io.Closer) *Store {
	store := &Store{
		dir: dir, logger: options.Logger, clock: options.Clock, manual: options.Manual, lock: lock,
		claimed: map[string]bool{}, done: make(chan struct{}),
	}
	if store.logger == nil {
		store.logger = slog.New(slog.DiscardHandler)
	}
	if store.clock == nil {
		store.clock = time.Now
	}
	if options.Memory > 0 {
		store.memory = &memory{capacity: options.Memory}
	}
	store.background, store.stop = context.WithCancel(context.WithoutCancel(ctx))
	return store
}

func (s *Store) logOpened() {
	s.logger.Info("store opened", "dir", s.dir)
	if !directoryLocking {
		s.logger.Warn("directory lock unavailable on this platform; open one store per directory", "dir", s.dir)
	}
}

// Close stops background work, closes every engine, the last opened first, and
// releases the directory. When ctx ends first Close returns its error, and the
// cleanup still finishes; a later Close waits for it.
func (s *Store) Close(ctx context.Context) error {
	s.closing.Do(func() { go s.shutdown(context.WithoutCancel(ctx)) })
	select {
	case <-s.done:
		return s.closeErr
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (s *Store) shutdown(ctx context.Context) {
	engines := s.refuseNewWork()

	s.stop()
	s.running.Wait()

	var err error
	for _, engine := range slices.Backward(engines) {
		err = errors.Join(err, engine.Close(ctx))
	}
	err = errors.Join(err, s.lock.Close())

	if err != nil {
		s.logger.Warn("store closed with errors", "dir", s.dir, "error", err)
	} else {
		s.logger.Info("store closed", "dir", s.dir)
	}
	s.closeErr = err
	close(s.done)
}

// refuseNewWork turns away claims, engines and background work from now on
func (s *Store) refuseNewWork() []Engine {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.closed = true
	return s.engines
}
