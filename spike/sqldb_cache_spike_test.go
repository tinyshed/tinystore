package spike

import (
	"container/list"
	"context"
	"fmt"
	"math/rand/v2"
	"path/filepath"
	"testing"
)

// sqldbFrequentCache is sqldbStatementCache with a door: a text that misses is
// kept only when it has been read more often, lately, than the text it would
// evict, counts halved every ten reads a statement kept; one that is not kept
// compiles, runs and finalizes in one step, as without a cache, so that texts
// passing through do not churn the ones that stay
type sqldbFrequentCache struct {
	sqldbStatementCache
	counts map[string]int
	reads  int
	misses int
}

func (c *sqldbFrequentCache) read(ctx context.Context, query, id string) error {
	c.count(query)
	if element, ok := c.statements[query]; ok {
		c.order.MoveToFront(element)
		cached, _ := element.Value.(*sqldbCached)
		return sqldbScanNote(cached.statement.QueryRowContext(ctx, id).Scan)
	}
	if c.admits(query) {
		return c.sqldbStatementCache.read(ctx, query, id)
	}
	c.misses++
	return sqldbScanNote(c.conn.QueryRowContext(ctx, query, id).Scan)
}

// admits is room left, or a text read more often than the one leaving
func (c *sqldbFrequentCache) admits(query string) bool {
	if c.order.Len() < c.size {
		return true
	}
	oldest, _ := c.order.Back().Value.(*sqldbCached)
	return c.counts[query] > c.counts[oldest.query]
}

func (c *sqldbFrequentCache) count(query string) {
	c.counts[query]++
	if c.reads++; c.reads < 10*c.size {
		return
	}
	c.reads = 0
	for text, n := range c.counts {
		if n /= 2; n == 0 {
			delete(c.counts, text)
		} else {
			c.counts[text] = n
		}
	}
}

// TestSQLDBCacheThatDoesNotChurn runs point reads over 16 to 1024 texts,
// uniformly, and over 1024 of which 64 take nine reads in ten, on one reader
// connection: compiled each call, through a least-recently-used cache of 128,
// and through the same cache with a door that keeps a text only when it is
// read more often than the one it would evict
func TestSQLDBCacheThatDoesNotChurn(t *testing.T) {
	sqldbMeasuring(t)
	const count, size = 100_000, 128
	path := filepath.Join(sqldbDir(t), "app.db")
	file := sqldbOpen(t, path, 1)
	sqldbCreate(t, file, count)
	ctx := t.Context()
	pool := kvReaderPool(t, path, "cache_size(-1024)")

	workloads := []struct {
		name  string
		texts int
		pick  func(random *rand.Rand) int
	}{
		{"16 texts, uniform", 16, func(r *rand.Rand) int { return r.IntN(16) }},
		{"128 texts, uniform", 128, func(r *rand.Rand) int { return r.IntN(128) }},
		{"1024 texts, uniform", 1024, func(r *rand.Rand) int { return r.IntN(1024) }},
		{"1024 texts, 64 hot", 1024, func(r *rand.Rand) int {
			if r.IntN(10) < 9 {
				return r.IntN(64)
			}
			return 64 + r.IntN(960)
		}},
	}
	for _, workload := range workloads {
		queries := make([]string, workload.texts)
		for i := range queries {
			queries[i] = fmt.Sprintf("%s /* read %d */", sqldbNoteByID, i)
		}
		for _, way := range []string{"compiled each call", "least recently used", "with a door"} {
			conn, err := pool.Conn(ctx)
			if err != nil {
				t.Fatal(err)
			}
			cache := &sqldbFrequentCache{
				sqldbStatementCache: sqldbStatementCache{
					conn: conn, size: size,
					statements: map[string]*list.Element{}, order: list.New(),
				},
				counts: map[string]int{},
			}
			random := rand.New(rand.NewPCG(3, 9))
			reads := 0
			result := kvLoad(1, sqldbSeconds(), func(int) error {
				reads++
				query, id := queries[workload.pick(random)], sqldbNoteID(random.IntN(count))
				switch way {
				case "compiled each call":
					return sqldbScanNote(conn.QueryRowContext(ctx, query, id).Scan)
				case "least recently used":
					return cache.sqldbStatementCache.read(ctx, query, id)
				}
				return cache.read(ctx, query, id)
			})
			compiled := cache.prepares + cache.misses
			if way == "compiled each call" {
				compiled = reads
			}
			t.Logf("%-20s %-20s compiled %5.1f%%  reads %s", workload.name, way,
				100*float64(compiled)/float64(reads), result)
			for _, element := range cache.statements {
				cached, _ := element.Value.(*sqldbCached)
				_ = cached.statement.Close()
			}
			if err = conn.Close(); err != nil {
				t.Fatal(err)
			}
		}
	}
}
