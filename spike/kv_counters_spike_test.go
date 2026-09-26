package spike

import (
	"database/sql"
	"fmt"
	"hash/maphash"
	"math/rand/v2"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// kvCounterAdd is a counter's Add as docs/kv.md writes it: an expired row
// counts as absent, and an overflow writes nothing rather than a REAL
const kvCounterAdd = `insert into cells (bucket, path, version, expires, value) values (?1, ?2, ?3, ?4, ?5)
	on conflict (bucket, path) do update set
		value   = iif(cells.expires <= ?6, excluded.value, cells.value + excluded.value),
		expires = iif(cells.expires <= ?6, excluded.expires, cells.expires),
		version = excluded.version
	where cells.expires <= ?6 or typeof(cells.value + excluded.value) = 'integer'`

// TestKVCounterFlush measures the two halves of a LoseAtMost counter: an Add
// that changes memory, from 1 to 512 goroutines over 100,000 distinct keys, and
// the flush that writes the deltas of 1,000 to 100,000 distinct keys in one
// transaction, first as new rows and then onto the rows it wrote
func TestKVCounterFlush(t *testing.T) {
	kvMeasuring(t)
	t.Run("add", func(t *testing.T) {
		keys := kvAttemptKeys(100_000)
		for _, workers := range []int{1, 8, 64, 512} {
			deltas := &kvDeltas{seed: maphash.MakeSeed()}
			randoms := make([]*rand.Rand, workers)
			for worker := range randoms {
				randoms[worker] = rand.New(rand.NewPCG(uint64(worker), 13))
			}
			result := kvLoad(workers, kvSeconds(), func(worker int) error {
				deltas.add(keys[randoms[worker].IntN(len(keys))], 1)
				return nil
			})
			t.Logf("%3d goroutines  Adds in memory %s  %d keys waiting", workers, result, deltas.waiting())
		}
	})
	t.Run("flush", func(t *testing.T) {
		for _, count := range []int{1_000, 10_000, 100_000} {
			file := kvOpen(t, filepath.Join(kvDir(t), "kv.db"), 4096, 2)
			keys := kvAttemptKeys(count)
			for _, rows := range []string{"new rows", "onto them"} {
				began := time.Now()
				kvFlush(t, file, keys, rows == "new rows")
				took := time.Since(began)
				t.Logf("%7d keys, %-9s  one transaction %8.1f ms  %9.0f keys/s", count, rows,
					float64(took)/float64(time.Millisecond), float64(count)/took.Seconds())
			}
		}
	})
}

// kvAttemptKeys are counters of attempts by address, as a flood brings them
func kvAttemptKeys(count int) []string {
	keys := make([]string, count)
	for i := range keys {
		keys[i] = string(kvPath(fmt.Sprintf("10.%d.%d.%d", i>>16&255, i>>8&255, i&255), "ip"))
	}
	return keys
}

// kvFlush writes a delta of one to every key in one transaction, and checks on
// the second flush that the deltas were added rather than replaced
func kvFlush(t *testing.T, file *sqlite.File, keys []string, fresh bool) {
	t.Helper()
	ctx := t.Context()
	now := time.Now()
	expires := now.Add(15 * time.Minute).UnixMilli()
	err := file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		for i, key := range keys {
			if _, err := w.ExecContext(ctx, kvCounterAdd, 1, []byte(key), i+1, expires, 1, now.UnixMilli()); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if fresh {
		return
	}
	var value int64
	err = file.View(ctx, func(tx *sql.Tx) error {
		return tx.QueryRowContext(ctx, `select value from cells where bucket = 1 and path = ?1`, []byte(keys[0])).Scan(&value)
	})
	if err != nil || value != 2 {
		t.Fatalf("a flushed counter holds %d, %v; want 2", value, err)
	}
}

// kvDeltas is what a LoseAtMost bucket holds between flushes: a delta a key, in
// shards, so that goroutines adding to different keys rarely meet
type kvDeltas struct {
	seed   maphash.Seed
	shards [64]kvShard
}

// kvShard is one lock and its keys, padded to a cache line of its own
type kvShard struct {
	mu     sync.Mutex
	deltas map[string]int64
	_      [48]byte
}

func (d *kvDeltas) add(key string, n int64) int64 {
	shard := &d.shards[maphash.String(d.seed, key)%uint64(len(d.shards))]
	shard.mu.Lock()
	defer shard.mu.Unlock()
	if shard.deltas == nil {
		shard.deltas = map[string]int64{}
	}
	shard.deltas[key] += n
	return shard.deltas[key]
}

func (d *kvDeltas) waiting() int {
	total := 0
	for i := range d.shards {
		d.shards[i].mu.Lock()
		total += len(d.shards[i].deltas)
		d.shards[i].mu.Unlock()
	}
	return total
}
