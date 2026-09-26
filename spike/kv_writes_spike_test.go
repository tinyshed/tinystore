package spike

import (
	"context"
	"path/filepath"
	"sync"
	"sync/atomic"
	"testing"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// kvSet writes one new session durably: it returns once its commit has
type kvSet func(i int, path, value []byte) error

// TestKVDurableSets measures a durable Set through internal/sqlite from 1 to
// 512 goroutines, into a file of 100,000 sessions: a transaction and an fsync a
// Set, against the Sets waiting behind a leader committed together
func TestKVDurableSets(t *testing.T) {
	kvMeasuring(t)
	ways := []struct {
		name string
		set  func(context.Context, *sqlite.File) kvSet
	}{
		{"one each", kvSetEach},
		{"grouped", kvSetGrouped},
	}
	for _, way := range ways {
		for _, workers := range []int{1, 8, 64, 512} {
			file := kvOpen(t, filepath.Join(kvDir(t), "kv.db"), 4096, 2)
			kvPreload(t, file, 100_000, 64)
			set := way.set(t.Context(), file)
			before := kvCommits(t, file)

			var next atomic.Int64
			result := kvLoad(workers, kvSeconds(), func(int) error {
				i := int(next.Add(1))
				return set(i, kvSession(kvSeedNew, i), kvValue(i, 64))
			})

			commits := kvCommits(t, file) - before
			t.Logf("%-8s %3d goroutines  Sets %s  %6d commits, %6.1f Sets each", way.name, workers, result,
				commits, float64(result.ops)/float64(max(commits, 1)))
		}
	}
}

func kvCommits(t *testing.T, file *sqlite.File) uint64 {
	t.Helper()
	counters, err := file.WriterCounters(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	return counters.Commits
}

// kvSetEach commits every Set on its own: the writer's slot, a transaction, an
// fsync
func kvSetEach(ctx context.Context, file *sqlite.File) kvSet {
	var revision atomic.Int64
	return func(i int, path, value []byte) error {
		return file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
			version := revision.Add(1)
			if _, err := w.ExecContext(ctx, kvUpsert, 1, path, version, kvExpiry(i), value); err != nil {
				return err
			}
			_, err := w.ExecContext(ctx, kvRevision, version)
			return err
		})
	}
}

// kvGroup is the group commit docs/kv.md leaves to this round, without a
// goroutine of its own: the first caller to find no leader commits every Set
// queued behind it, each in a savepoint, then hands the lead to the first
// caller still waiting
type kvGroup struct {
	ctx      context.Context
	file     *sqlite.File
	revision int64 // the leader's alone

	mu      sync.Mutex
	queue   []*kvWaiting
	leading bool
}

type kvWaiting struct {
	i           int
	path, value []byte
	done        chan kvOutcome
}

// kvOutcome is a waiting Set's commit, or the lead handed to it
type kvOutcome struct {
	err  error
	lead bool
}

// a group commits at most this many Sets in one transaction
const kvGroupLimit = 1024

func kvSetGrouped(ctx context.Context, file *sqlite.File) kvSet {
	group := &kvGroup{ctx: context.WithoutCancel(ctx), file: file}
	return group.set
}

func (g *kvGroup) set(i int, path, value []byte) error {
	waiting := &kvWaiting{i: i, path: path, value: value, done: make(chan kvOutcome, 1)}
	g.mu.Lock()
	g.queue = append(g.queue, waiting)
	follows := g.leading
	g.leading = true
	g.mu.Unlock()

	if follows {
		outcome := <-waiting.done
		if !outcome.lead {
			return outcome.err
		}
	}
	return g.lead(waiting)
}

// lead commits the head of the queue, where the leader's own Set is, answers
// every Set in it and hands the lead on
func (g *kvGroup) lead(own *kvWaiting) error {
	g.mu.Lock()
	batch := g.queue[:min(len(g.queue), kvGroupLimit)]
	g.queue = g.queue[len(batch):]
	g.mu.Unlock()

	err := g.file.UpdatePrepared(g.ctx, func(w sqlite.Writer) error {
		for _, waiting := range batch {
			if err := g.write(w, waiting); err != nil {
				return err
			}
		}
		_, err := w.ExecContext(g.ctx, kvRevision, g.revision)
		return err
	})

	for _, waiting := range batch {
		if waiting != own {
			waiting.done <- kvOutcome{err: err}
		}
	}
	g.handOn()
	return err
}

// write is one Set in a savepoint, so that a Set refused alone would roll back
// alone
func (g *kvGroup) write(w sqlite.Writer, waiting *kvWaiting) error {
	g.revision++
	if _, err := w.ExecContext(g.ctx, "savepoint kv_set"); err != nil {
		return err
	}
	if _, err := w.ExecContext(g.ctx, kvUpsert, 1, waiting.path, g.revision, kvExpiry(waiting.i),
		waiting.value); err != nil {
		return err
	}
	_, err := w.ExecContext(g.ctx, "release kv_set")
	return err
}

// handOn wakes the first caller still waiting as the next leader, or frees the
// lead when nobody waits
func (g *kvGroup) handOn() {
	g.mu.Lock()
	defer g.mu.Unlock()
	if len(g.queue) == 0 {
		g.leading = false
		g.queue = nil
		return
	}
	g.queue[0].done <- kvOutcome{lead: true}
}
