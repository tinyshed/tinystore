package tinystore

import (
	"context"
	"slices"
	"sync"
)

// MemoryUsage is the store's budget: Capacity is Options.Memory, zero when
// every engine keeps only its own per-call limits.
type MemoryUsage struct {
	Used, Peak, Capacity int64
}

// Reservation is bytes of the store's memory that one piece of work holds.
// The zero Reservation holds nothing, for work that needs no memory.
type Reservation struct {
	memory *memory // nil when the store has no Options.Memory
	mu     sync.Mutex
	bytes  int64
}

// unbudgeted is every reservation of a store without Options.Memory
var unbudgeted Reservation

// Reserve waits, in arrival order, until bytes fit the store's memory. A
// reservation larger than the whole budget is refused at once with ErrLimit,
// and a store without Options.Memory grants every reservation.
func (s *Store) Reserve(ctx context.Context, bytes int64) (*Reservation, error) {
	if s.memory == nil {
		return &unbudgeted, nil
	}
	if err := s.memory.acquire(ctx, bytes); err != nil {
		return nil, err
	}
	return &Reservation{memory: s.memory, bytes: bytes}, nil
}

// ReserveNow takes bytes only if they fit at once, ahead of any reservation
// waiting, and is ErrLimit otherwise. Work holding what a waiting reservation
// may need, such as a file's writer, reserves this way: waiting, it could wait
// for itself.
func (s *Store) ReserveNow(bytes int64) (*Reservation, error) {
	if s.memory == nil {
		return &unbudgeted, nil
	}
	if err := s.memory.acquireNow(bytes); err != nil {
		return nil, err
	}
	return &Reservation{memory: s.memory, bytes: bytes}, nil
}

// Shrink gives back all but n of the bytes, for work that reserved its worst
// case and has learnt what it holds.
func (r *Reservation) Shrink(n int64) {
	if r.memory == nil {
		return
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if kept := max(n, 0); kept < r.bytes {
		r.memory.release(r.bytes - kept)
		r.bytes = kept
	}
}

// Release gives back what the reservation holds; a second call gives nothing.
func (r *Reservation) Release() {
	r.Shrink(0)
}

// Readers is how many reader connections an engine that wants want opens:
// want, or Options.Readers when that is fewer.
func (s *Store) Readers(want int) int {
	if s.readers > 0 && s.readers < want {
		return s.readers
	}
	return want
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
	if err := m.fits(bytes); err != nil {
		return err
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

// acquireNow takes bytes if they fit beside what is held; the reservations
// waiting wait for more than is free, or they would have been granted
func (m *memory) acquireNow(bytes int64) error {
	if err := m.fits(bytes); err != nil {
		return err
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	if free := m.capacity - m.used; bytes > free {
		return &LimitError{Name: "store memory, now", Wanted: m.used + bytes, Bound: m.capacity}
	}
	m.take(bytes)
	return nil
}

func (m *memory) fits(bytes int64) error {
	if bytes <= 0 || bytes > m.capacity {
		return &LimitError{Name: "store memory", Wanted: bytes, Bound: m.capacity}
	}
	return nil
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
