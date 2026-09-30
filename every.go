package tinystore

import (
	"context"
	"errors"
	"log/slog"
	"time"
)

// a failure repeated within this long is counted rather than logged again
const quietFailures = 10 * time.Minute

// Every runs work at each interval until the store closes, and soon asks for
// its next run now rather than at the next interval: asks that come while it
// runs or waits to run make one run more. A Manual store runs nothing, and its
// soon does nothing; a failure is logged, and the next run still happens.
func (s *Store) Every(name string, interval time.Duration, work func(context.Context) error) (soon func()) {
	return s.every(s.logger, name, interval, work)
}

// EveryEngine runs engine work with the same engine attribute as Store.Logger.
func (s *Store) EveryEngine(engine, name string, interval time.Duration, work func(context.Context) error) (
	soon func(),
) {
	return s.every(s.Logger(engine), name, interval, work)
}

func (s *Store) every(logger *slog.Logger, name string, interval time.Duration,
	work func(context.Context) error,
) (soon func()) {
	if s.manual {
		return func() {}
	}
	if interval <= 0 {
		logger.Warn("background work refused: interval must be positive", "work", name, "interval", interval)
		return func() {}
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return func() {}
	}
	failures := &failureLog{logger: logger, work: name, quiet: quietFailures}
	asked := make(chan struct{}, 1)
	s.running.Go(func() { s.repeat(interval, work, failures, asked) })
	return func() {
		select {
		case asked <- struct{}{}:
		default:
		}
	}
}

func (s *Store) repeat(interval time.Duration, work func(context.Context) error, failures *failureLog,
	asked <-chan struct{},
) {
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		select {
		case <-s.background.Done():
			return
		case <-ticker.C:
		case <-asked:
		}
		failures.observe(s.clock(), work(s.background))
	}
}

// failureLog logs a failure, a changed one, and a repeated one once per quiet
// period, then one line when the work recovers:
//
//	00:00  disk full        → Warn "background work failed" failures=1
//	00:01…00:09 the same    → counted
//	00:10  disk full        → Warn failures=11
//	00:11  success          → Info "background work recovered" failures=11
type failureLog struct {
	logger   *slog.Logger
	work     string
	quiet    time.Duration
	last     string
	loggedAt time.Time
	failures int
}

func (f *failureLog) observe(now time.Time, err error) {
	if errors.Is(err, context.Canceled) {
		return
	}
	if err == nil {
		f.recovered()
		return
	}

	f.failures++
	if err.Error() == f.last && now.Sub(f.loggedAt) < f.quiet {
		return
	}
	f.logger.Warn("background work failed", "work", f.work, "error", err, "failures", f.failures)
	f.last, f.loggedAt = err.Error(), now
}

func (f *failureLog) recovered() {
	if f.failures > 0 {
		f.logger.Info("background work recovered", "work", f.work, "failures", f.failures)
	}
	f.last, f.failures = "", 0
}
