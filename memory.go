package tinystore

import (
	"context"
	"fmt"
	"slices"
	"sync"
)

// MemoryUsage is the store's budget: Capacity is Options.Memory, zero when
// every engine keeps only its own per-call limits.
type MemoryUsage struct {
	Used, Peak, Capacity int64
}

// Reserve waits, in arrival order, until bytes fit the store's memory, and
// returns the release that gives them back; call it once. A reservation larger
// than the whole budget is refused at once with ErrLimit, and a store without
// Options.Memory grants every reservation.
func (s *Store) Reserve(ctx context.Context, bytes int64) (release func(), err error) {
	if s.memory == nil {
		return func() {}, nil
	}
	if err := s.memory.acquire(ctx, bytes); err != nil {
		return nil, err
	}
	return sync.OnceFunc(func() { s.memory.release(bytes) }), nil
}

func (s *Store) Memory() MemoryUsage {
	if s.memory == nil {
		return MemoryUsage{}
	}
	return s.memory.usage()
}

// memory grants reservations in the order they were asked for, so a large one
// is never passed over by the small ones that arrive after it
type memory struct {
	mu                   sync.Mutex
	capacity, used, peak int64
	waiting              []*memoryWaiter
}

// memoryWaiter is a reservation that did not fit; granted closes when release
// has made room for it and taken its bytes on its behalf
type memoryWaiter struct {
	bytes   int64
	granted chan struct{}
}

func (m *memory) usage() MemoryUsage {
	m.mu.Lock()
	defer m.mu.Unlock()
	return MemoryUsage{Used: m.used, Peak: m.peak, Capacity: m.capacity}
}

func (m *memory) acquire(ctx context.Context, bytes int64) error {
	if bytes <= 0 || bytes > m.capacity {
		return fmt.Errorf("%w: a reservation of %d bytes against the store's %d", ErrLimit, bytes, m.capacity)
	}
	m.mu.Lock()
	if err := ctx.Err(); err != nil {
		m.mu.Unlock()
		return err
	}
	if len(m.waiting) == 0 && bytes <= m.capacity-m.used {
		m.take(bytes)
		m.mu.Unlock()
		return nil
	}
	waiter := &memoryWaiter{bytes: bytes, granted: make(chan struct{})}
	m.waiting = append(m.waiting, waiter)
	m.mu.Unlock()

	select {
	case <-waiter.granted:
		if err := ctx.Err(); err != nil {
			m.abandon(waiter)
			return err
		}
		return nil
	case <-ctx.Done():
		m.abandon(waiter)
		return ctx.Err()
	}
}

// abandon takes a cancelled waiter out of the queue, or gives its bytes back
// when release granted them in the meantime
func (m *memory) abandon(waiter *memoryWaiter) {
	m.mu.Lock()
	defer m.mu.Unlock()
	select {
	case <-waiter.granted:
		m.used -= waiter.bytes
	default:
		m.waiting = slices.DeleteFunc(m.waiting, func(queued *memoryWaiter) bool { return queued == waiter })
	}
	m.grant()
}

func (m *memory) release(bytes int64) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.used -= bytes
	m.grant()
}

// grant serves waiters in arrival order and stops at the first that does not
// fit, even when a later one would
func (m *memory) grant() {
	for len(m.waiting) > 0 && m.waiting[0].bytes <= m.capacity-m.used {
		waiter := m.waiting[0]
		m.waiting = m.waiting[1:]
		m.take(waiter.bytes)
		close(waiter.granted)
	}
}

func (m *memory) take(bytes int64) {
	m.used += bytes
	m.peak = max(m.peak, m.used)
}
