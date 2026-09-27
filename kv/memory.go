package kv

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
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
// holds it is the truth and the file is behind it by what changed since the
// last flush; a key it does not hold is as the file has it.
type memory struct {
	state  *Store
	bucket int64
	name   string
	bound  int64 // counters memory may hold, changed or not, before a new one waits for a flush
	seed   maphash.Seed
	shards [memoryShards]shard

	// a change that reads the file holds gate shared until it has kept what it
	// read; eviction and Clear hold it alone, so that nothing keeps a value the
	// file held before them
	gate     sync.RWMutex
	held     atomic.Int64 // counters in the shards, and the places taken for ones on their way
	flushing sync.Mutex   // one flush at a time, and none while a Clear runs
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
	m := &memory{state: state, bucket: bucket, name: name, bound: maxHeld, seed: maphash.MakeSeed()}
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
	value int64, err error,
) {
	err = m.withRoom(ctx, func() (full bool, err error) {
		value, full, err = m.tryChange(ctx, c, created, next)
		return full, err
	})
	return value, err
}

// forget marks the counter at c's path absent, to be deleted by the next flush
func (m *memory) forget(ctx context.Context, c call) error {
	return m.withRoom(ctx, func() (bool, error) { return m.tryForget(c), nil })
}

// withRoom runs try until it finds room for a counter it adds, flushing when
// memory holds as many as it may, so that memory lets go of what the file
// then holds as memory does
func (m *memory) withRoom(ctx context.Context, try func() (full bool, err error)) error {
	for {
		full, err := try()
		if !full || err != nil {
			return err
		}
		if _, err = m.flush(ctx); err != nil {
			return err
		}
	}
}

// tryChange is change while memory has room for a counter it has to add; full
// says that it had none and changed nothing
func (m *memory) tryChange(ctx context.Context, c call, created int64, next func(int64) (int64, error)) (
	value int64, full bool, err error,
) {
	m.gate.RLock()
	defer m.gate.RUnlock()

	path := string(c.path)
	target := m.shardOf(path)
	if !target.holds(path) {
		if !m.takeRoom() {
			return 0, true, nil
		}
		loaded, readErr := m.readFile(ctx, c)
		if readErr != nil || !target.keep(path, loaded) {
			m.held.Add(-1)
		}
		if readErr != nil {
			return 0, false, readErr
		}
	}

	value, err = target.change(path, c.now, created, next)
	return value, false, err
}

// tryForget is forget while memory has room for a counter it has to add
func (m *memory) tryForget(c call) (full bool) {
	m.gate.RLock()
	defer m.gate.RUnlock()

	path := string(c.path)
	target := m.shardOf(path)
	if target.holds(path) {
		target.forget(path)
		return false
	}
	if !m.takeRoom() {
		return true
	}
	if !target.forget(path) {
		m.held.Add(-1)
	}
	return false
}

// takeRoom takes a place for one more counter below the bound, and says
// whether there was one; places are taken one at a time, so that callers
// arriving together cannot pass the bound between a look and an add
func (m *memory) takeRoom() bool {
	for {
		held := m.held.Load()
		if held >= m.bound {
			return false
		}
		if m.held.CompareAndSwap(held, held+1) {
			return true
		}
	}
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

// keep holds what the file had for path, and says whether it did: another
// change may have kept it first
func (s *shard) keep(path string, loaded counter) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.counters[path]; ok {
		return false
	}
	s.counters[path] = &loaded
	return true
}

func (s *shard) change(path string, now, created int64, next func(int64) (int64, error)) (int64, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	held := s.counters[path]
	current, expires := int64(0), created
	if held.live(now) {
		current, expires = held.value, held.expires
	}
	value, err := next(current)
	if err != nil {
		return 0, err
	}
	*held = counter{value: value, expires: expires, dirty: true}
	return value, nil
}

// forget marks path absent, and says whether it added the counter to do so
func (s *shard) forget(path string) (added bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	held, ok := s.counters[path]
	if !ok {
		held = &counter{}
		s.counters[path] = held
	}
	*held = counter{absent: true, dirty: true}
	return !ok
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
	where bucket = ?1 and path = ?2 and (expires is null or expires > ?3) and not ` + hiddenCells

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

// flushBatch writes at most flushBatch changed counters in one transaction
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
		if target.counters[taken.path] == taken.entry {
			taken.entry.dirty = true
		}
		target.mu.Unlock()
	}
}

// clear runs a Clear of the branch under prefix in a transaction of its own,
// and lets go of what memory holds under it once the Clear commits. Flushes
// and changes wait throughout, and reads of memory from the commit until
// memory has let go, so that no flush writes a cleared counter again and no
// read finds one. A Clear that rolled back leaves memory as it was; one whose
// commit failed may be in the file, so memory lets go as a crash would.
func (m *memory) clear(ctx context.Context, prefix []byte, clearIn func(sqlite.Writer) error) error {
	m.flushing.Lock()
	defer m.flushing.Unlock()
	m.gate.Lock()
	defer m.gate.Unlock()

	committing := false
	err := m.state.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		if err := clearIn(w); err != nil {
			return err
		}
		m.lockShards()
		committing = true
		return nil
	})
	if !committing {
		return err
	}
	m.forgetUnder(prefix)
	m.unlockShards()
	if err != nil {
		return fmt.Errorf("%w: %w", ErrOutcomeUnknown, err)
	}
	return nil
}

func (m *memory) lockShards() {
	for i := range m.shards {
		m.shards[i].mu.Lock()
	}
}

func (m *memory) unlockShards() {
	for i := range m.shards {
		m.shards[i].mu.Unlock()
	}
}

// forgetUnder lets go of the counters under a branch, changed or not; its
// caller holds every shard
func (m *memory) forgetUnder(prefix []byte) {
	for i := range m.shards {
		for path := range m.shards[i].counters {
			if isUnder(path, prefix) {
				delete(m.shards[i].counters, path)
				m.held.Add(-1)
			}
		}
	}
}

// isUnder says that path lies in the branch prefix names or below it: the
// prefix, then an owner's mark or a key's
func isUnder(path string, prefix []byte) bool {
	if len(path) <= len(prefix) || path[:len(prefix)] != string(prefix) {
		return false
	}
	return path[len(prefix)] == ownerMark || path[len(prefix)] == keyMark
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
		before := len(target.counters)
		maps.DeleteFunc(target.counters, func(_ string, entry *counter) bool { return !entry.dirty })
		m.held.Add(int64(len(target.counters) - before))
		target.mu.Unlock()
	}
}
