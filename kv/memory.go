package kv

import (
	"context"
	"database/sql"
	"errors"
	"hash/maphash"
	"maps"
	"sync"
	"sync/atomic"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// the shards a memory spreads its keys over, so that goroutines changing
// different keys rarely wait for one another
const memoryShards = 64

// memory is what LoseAtMost counters hold between flushes. For the keys it
// holds it is the truth and the file is behind it by an interval at most; a
// key it does not hold is as the file has it.
type memory struct {
	state  *Store
	bucket int64
	name   string
	bound  int64 // counters that may wait for a flush before a change flushes them
	seed   maphash.Seed
	shards [memoryShards]shard

	// a change that reads the file holds gate shared until it has kept what it
	// read; eviction and Clear hold it alone, so that nothing keeps a value the
	// file held before them
	gate     sync.RWMutex
	waiting  atomic.Int64 // counters changed since the flush that wrote them
	flushing sync.Mutex
}

type shard struct {
	mu       sync.Mutex
	counters map[string]*counter
	_        [48]byte // a cache line of its own
}

// counter is one key in memory: absent when a Delete said so or the file held
// none, dirty until a flush has written it
type counter struct {
	value   int64
	expires int64 // unix milliseconds, 0 for never
	absent  bool
	dirty   bool
}

func (c *counter) live(now int64) bool {
	return !c.absent && (c.expires == 0 || c.expires > now)
}

func (c *counter) valueAt(now int64) int64 {
	if !c.live(now) {
		return 0
	}
	return c.value
}

func newMemory(state *Store, bucket int64, name string) *memory {
	m := &memory{state: state, bucket: bucket, name: name, bound: maxWaiting, seed: maphash.MakeSeed()}
	for i := range m.shards {
		m.shards[i].counters = map[string]*counter{}
	}
	return m
}

func (m *memory) shardOf(path string) *shard {
	return &m.shards[maphash.String(m.seed, path)%memoryShards]
}

// change applies next to the counter at c's path and returns what it holds
// now; a counter it finds absent or expired starts from zero with the expiry
// created, 0 for never
func (m *memory) change(ctx context.Context, c call, created int64, next func(held int64) (int64, error)) (
	int64, error,
) {
	if err := m.makeRoom(ctx); err != nil {
		return 0, err
	}
	m.gate.RLock()
	defer m.gate.RUnlock()

	path := string(c.path)
	target := m.shardOf(path)
	if !target.holds(path) {
		loaded, err := m.readFile(ctx, c)
		if err != nil {
			return 0, err
		}
		target.keep(path, loaded)
	}

	value, newlyDirty, err := target.change(path, c.now, created, next)
	if newlyDirty {
		m.waiting.Add(1)
	}
	return value, err
}

// forget marks the counter at c's path absent, to be deleted by the next flush
func (m *memory) forget(ctx context.Context, c call) error {
	if err := m.makeRoom(ctx); err != nil {
		return err
	}
	m.gate.RLock()
	defer m.gate.RUnlock()

	if m.shardOf(string(c.path)).forget(string(c.path)) {
		m.waiting.Add(1)
	}
	return nil
}

// get is what the counter at c's path holds, from memory or else the file
func (m *memory) get(ctx context.Context, c call) (int64, error) {
	if held, ok := m.shardOf(string(c.path)).valueAt(string(c.path), c.now); ok {
		return held, nil
	}
	loaded, err := m.readFile(ctx, c)
	return loaded.valueAt(c.now), err
}

func (s *shard) holds(path string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	_, ok := s.counters[path]
	return ok
}

// keep holds what the file had for path, unless another change kept it first
func (s *shard) keep(path string, loaded counter) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.counters[path]; !ok {
		s.counters[path] = &loaded
	}
}

func (s *shard) change(path string, now, created int64, next func(int64) (int64, error)) (
	value int64, newlyDirty bool, err error,
) {
	s.mu.Lock()
	defer s.mu.Unlock()
	held := s.counters[path]
	current, expires := int64(0), created
	if held.live(now) {
		current, expires = held.value, held.expires
	}
	if value, err = next(current); err != nil {
		return 0, false, err
	}
	newlyDirty = !held.dirty
	*held = counter{value: value, expires: expires, dirty: true}
	return value, newlyDirty, nil
}

func (s *shard) forget(path string) (newlyDirty bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	held, ok := s.counters[path]
	if !ok {
		held = &counter{}
		s.counters[path] = held
	}
	newlyDirty = !held.dirty
	*held = counter{absent: true, dirty: true}
	return newlyDirty
}

func (s *shard) valueAt(path string, now int64) (int64, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	held, ok := s.counters[path]
	if !ok {
		return 0, false
	}
	return held.valueAt(now), true
}

const selectCounterRow = `select value, expires from cells
	where bucket = ?1 and path = ?2 and (expires is null or expires > ?3)`

// readFile is the counter at c's path as the file has it, absent when the
// file has none or it expired
func (m *memory) readFile(ctx context.Context, c call) (counter, error) {
	var value any
	var expires sql.NullInt64
	err := m.state.file.Lookup(ctx, func(r sqlite.Reader) error {
		return sqlite.QueryRow(ctx, r, selectCounterRow, m.bucket, c.path, c.now).Scan(&value, &expires)
	})
	if errors.Is(err, sql.ErrNoRows) {
		return counter{absent: true}, nil
	}
	if err != nil {
		return counter{}, err
	}
	held, err := counted(value)
	return counter{value: held, expires: expires.Int64}, err
}

// makeRoom flushes before a change once the counters waiting reach the bound,
// so that memory stays bounded however many keys arrive
func (m *memory) makeRoom(ctx context.Context) error {
	if m.waiting.Load() < m.bound {
		return nil
	}
	_, err := m.flush(ctx)
	return err
}

// flushed is one changed counter as a flush took it, and the entry it came from
type flushed struct {
	path  string
	held  counter
	entry *counter
}

const writeCounter = `insert into cells (bucket, path, version, expires, value) values (?1, ?2, ?3, ?4, ?5)
	on conflict (bucket, path) do update set
		version = excluded.version, expires = excluded.expires, value = excluded.value`

// flush writes every counter changed since the last flush, flushBatch a
// transaction, then lets go of those that did not change again; it returns
// how many it wrote
func (m *memory) flush(ctx context.Context) (int, error) {
	m.flushing.Lock()
	defer m.flushing.Unlock()

	written := 0
	for {
		wrote, err := m.flushBatch(ctx)
		written += wrote
		if err != nil {
			return written, err
		}
		if wrote < flushBatch {
			break
		}
	}

	m.evict()
	return written, nil
}

func (m *memory) flushInBackground(ctx context.Context) error {
	_, err := m.flush(ctx)
	return err
}

// flushBatch writes at most flushBatch changed counters in one transaction.
// It takes them inside the transaction, so that a Clear, which drops them in
// a transaction of its own, runs wholly before it or wholly after.
func (m *memory) flushBatch(ctx context.Context) (int, error) {
	var batch []flushed
	err := m.state.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		batch = m.takeChanged(flushBatch)
		if len(batch) == 0 {
			return nil
		}
		version, err := m.state.nextRevision(ctx, w)
		for _, taken := range batch {
			if err == nil {
				err = m.write(ctx, w, taken, version)
			}
		}
		return err
	})
	if err != nil {
		m.giveBack(batch)
		return 0, err
	}
	return len(batch), nil
}

func (m *memory) write(ctx context.Context, w sqlite.Writer, taken flushed, version int64) error {
	if taken.held.absent {
		_, err := w.ExecContext(ctx, deleteCounter, m.bucket, []byte(taken.path))
		return err
	}
	expires := sql.NullInt64{Int64: taken.held.expires, Valid: taken.held.expires != 0}
	_, err := w.ExecContext(ctx, writeCounter, m.bucket, []byte(taken.path), version, expires, taken.held.value)
	return err
}

// takeChanged takes at most limit changed counters and marks them written; a
// change after it marks one changed again
func (m *memory) takeChanged(limit int) []flushed {
	var batch []flushed
	for i := range m.shards {
		batch = m.shards[i].takeChanged(batch, limit)
		if len(batch) == limit {
			break
		}
	}
	m.waiting.Add(-int64(len(batch)))
	return batch
}

func (s *shard) takeChanged(batch []flushed, limit int) []flushed {
	s.mu.Lock()
	defer s.mu.Unlock()
	for path, entry := range s.counters {
		if len(batch) == limit {
			break
		}
		if entry.dirty {
			entry.dirty = false
			batch = append(batch, flushed{path: path, held: *entry, entry: entry})
		}
	}
	return batch
}

// giveBack marks changed again what a failed flush took, unless a change or
// a Clear has come to it since
func (m *memory) giveBack(batch []flushed) {
	for _, taken := range batch {
		target := m.shardOf(taken.path)
		target.mu.Lock()
		if target.counters[taken.path] == taken.entry && !taken.entry.dirty {
			taken.entry.dirty = true
			m.waiting.Add(1)
		}
		target.mu.Unlock()
	}
}

// evict lets go of the counters the file now holds as memory does. It waits
// for the changes reading the file, so that none keeps what it read before
// the flush it follows.
func (m *memory) evict() {
	m.gate.Lock()
	defer m.gate.Unlock()
	for i := range m.shards {
		target := &m.shards[i]
		target.mu.Lock()
		maps.DeleteFunc(target.counters, func(_ string, entry *counter) bool { return !entry.dirty })
		target.mu.Unlock()
	}
}
