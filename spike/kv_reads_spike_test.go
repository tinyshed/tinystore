package spike

import (
	"context"
	"database/sql"
	"errors"
	"math/rand/v2"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// kvGetter looks one key up and says whether it is there
type kvGetter func(path []byte) (bool, error)

// TestKVPointReads measures a point Get from 1 to 512 goroutines over eight
// readers, in a file of one million sessions and, with TINYSTORE_KV_LARGE=1, ten
// million: through File.ViewPrepared, which begins a transaction for its one
// statement; the statement alone on the same kind of connection; and the
// statement with a 64 MiB page cache a connection instead of 1 MiB. Hits read
// stored sessions, misses keys the file does not hold, as a revocation list is
// mostly asked about
func TestKVPointReads(t *testing.T) {
	kvMeasuring(t)
	counts := []int{1_000_000}
	if os.Getenv("TINYSTORE_KV_LARGE") == "1" {
		counts = append(counts, 10_000_000)
	}
	for _, count := range counts {
		path := filepath.Join(kvDir(t), "kv.db")
		file := kvOpen(t, path, 4096, kvReaders)
		began := time.Now()
		kvPreload(t, file, count, 64)
		t.Logf("%d sessions written in %v; %d MiB with the log", count, time.Since(began).Round(time.Second),
			kvFileBytes(path)>>20)

		ways := []struct {
			name string
			get  kvGetter
		}{
			{"View", kvGetThroughView(t.Context(), file)},
			{"statement", kvGetThroughStatement(t, path, "cache_size(-1024)")},
			{"statement, 64 MiB", kvGetThroughStatement(t, path, "cache_size(-65536)")},
		}
		for _, lookup := range []struct {
			name string
			seed uint64
		}{{"hit", kvSeedStored}, {"miss", kvSeedAbsent}} {
			for _, way := range ways {
				for _, workers := range []int{1, 8, 64, 512} {
					randoms := make([]*rand.Rand, workers)
					for worker := range randoms {
						randoms[worker] = rand.New(rand.NewPCG(uint64(worker), 11))
					}
					result := kvLoad(workers, kvSeconds(), func(worker int) error {
						found, err := way.get(kvSession(lookup.seed, randoms[worker].IntN(count)))
						if err == nil && found != (lookup.seed == kvSeedStored) {
							return errKVWrongAnswer
						}
						return err
					})
					t.Logf("%8d keys  %-4s  %-17s %3d goroutines  Gets %s", count, lookup.name, way.name, workers, result)
				}
			}
		}
	}
}

func kvGetThroughView(ctx context.Context, file *sqlite.File) kvGetter {
	return func(path []byte) (bool, error) {
		found := false
		err := file.ViewPrepared(ctx, func(r sqlite.Reader) error {
			var (
				version        int64
				expires, spill sql.NullInt64
				value          []byte
			)
			err := sqlite.QueryRow(ctx, r, kvGet, 1, path, time.Now().UnixMilli()).Scan(&version, &expires, &value, &spill)
			if errors.Is(err, sql.ErrNoRows) {
				return nil
			}
			found = err == nil
			return err
		})
		return found, err
	}
}

func kvGetThroughStatement(t *testing.T, path, cache string) kvGetter {
	t.Helper()
	ctx := t.Context()
	statement, err := kvReaderPool(t, path, cache).PrepareContext(ctx, kvGet)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = statement.Close() })
	return func(path []byte) (bool, error) {
		return kvGetOn(ctx, statement, path)
	}
}
