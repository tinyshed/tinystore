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

	"github.com/tinyshed/tinystore/internal/dirlock"
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

	// Guest opens a directory another store holds, without its LOCK, as a
	// second process beside a running program opens it to reset a password:
	// nothing runs in the background, and only SQL databases open in it,
	// applying no migration and changing no schema, nor the store's own tables.
	// SQLite's locks share each file's writer between the two processes, and a
	// guest's write waits up to five seconds for the owner's.
	Guest bool

	// Readers bounds the reader connections each engine's file opens under
	// load, half a MiB each; zero leaves each engine its own count. A reader
	// beyond one closes after a minute unused, whatever the bound.
	Readers int

	// SelfMetrics periodically writes available engine reports into an opened
	// metrics engine. Manual stores call FlushSelfMetrics themselves.
	SelfMetrics bool
}

type Store struct {
	dir     string
	logger  *slog.Logger
	clock   func() time.Time
	manual  bool
	lock    io.Closer
	memory  *memory
	readers int
	guest   bool
	self    *selfMetrics

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
	if dir == "" || options.Memory < 0 || options.Readers < 0 {
		return nil, fmt.Errorf("%w: empty directory, negative memory or negative readers", ErrInvalid)
	}
	if options.Guest {
		return openGuest(ctx, dir, options)
	}
	if err := os.MkdirAll(dir, 0o750); err != nil {
		return nil, fmt.Errorf("create %s: %w", dir, err)
	}

	held, err := dirlock.Hold(dir)
	if errors.Is(err, dirlock.ErrHeld) {
		return nil, fmt.Errorf("%w: %s is open in another store", ErrInUse, dir)
	}
	if err != nil {
		return nil, fmt.Errorf("lock %s: %w", dir, err)
	}

	store := newStore(ctx, dir, options, held)
	store.logOpened()
	return store, nil
}

// openGuest opens a store's directory beside the store that holds it, taking
// no lock and making nothing: a guest of a directory that is not there is refused.
func openGuest(ctx context.Context, dir string, options Options) (*Store, error) {
	if info, err := os.Stat(dir); err != nil || !info.IsDir() {
		return nil, fmt.Errorf("%w: a guest of %s, which is no store's directory", ErrInvalid, dir)
	}
	options.Manual, options.SelfMetrics = true, false
	store := newStore(ctx, dir, options, nothingHeld{})
	store.guest = true
	store.logger.Info("store opened as a guest", "dir", dir)
	return store, nil
}

// nothingHeld is a guest's lock, which holds nothing
type nothingHeld struct{}

func (nothingHeld) Close() error { return nil }

// Guest says whether the store was opened with Options.Guest.
func (s *Store) Guest() bool { return s.guest }

func newStore(ctx context.Context, dir string, options Options, lock io.Closer) *Store {
	store := &Store{
		dir: dir, logger: options.Logger, clock: options.Clock, manual: options.Manual, lock: lock,
		readers: options.Readers, claimed: map[string]bool{}, done: make(chan struct{}),
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
	if options.SelfMetrics {
		store.self = &selfMetrics{slot: make(chan struct{}, 1)}
		store.self.soon = store.EveryEngine("metrics", "self-metrics", selfInterval, store.FlushSelfMetrics)
	}
	return store
}

func (s *Store) logOpened() {
	s.logger.Info("store opened", "dir", s.dir)
	if !dirlock.Supported {
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

	// a report that did not reach the metrics engine loses no data of the
	// application's, so it is logged rather than made a failure of Close
	if s.self != nil {
		if selfErr := s.flushSelf(ctx, true); selfErr != nil {
			s.Logger("metrics").Warn("background work failed", "work", "self-metrics", "error", selfErr)
		}
	}
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
