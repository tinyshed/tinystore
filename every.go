package tinystore

import (
	"context"
	"errors"
	"log/slog"
	"time"
)

// a failure repeated within this long is counted rather than logged again
const quietFailures = 10 * time.Minute

// Every runs work at each interval until the store closes. A Manual store runs
// nothing; a failure is logged, and the next run still happens.
func (s *Store) Every(name string, interval time.Duration, work func(context.Context) error) {
	if s.manual {
		return
	}
	if interval <= 0 {
		s.logger.Warn("background work refused: interval must be positive", "work", name, "interval", interval)
		return
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	failures := &failureLog{logger: s.logger, work: name, quiet: quietFailures}
	s.running.Go(func() { s.repeat(interval, work, failures) })
}

func (s *Store) repeat(interval time.Duration, work func(context.Context) error, failures *failureLog) {
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		select {
		case <-s.background.Done():
			return
		case <-ticker.C:
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
