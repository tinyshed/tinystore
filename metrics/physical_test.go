package metrics

import (
	"context"
	"database/sql"
	"encoding/binary"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

func TestPhysicalWriteCounters(t *testing.T) {
	if os.Getenv("TINYSTORE_PHYSICAL") != "1" {
		t.Skip("set TINYSTORE_PHYSICAL=1 to measure SQLite writes")
	}
	pageSize := 4096
	registerBatch := 100
	if value := os.Getenv("TINYSTORE_PAGE_SIZE"); value != "" {
		var err error
		pageSize, err = strconv.Atoi(value)
		if err != nil || (pageSize != 4096 && pageSize != 8192) {
			t.Fatal("TINYSTORE_PAGE_SIZE must be 4096 or 8192")
		}
	}
	if value := os.Getenv("TINYSTORE_REGISTER_BATCH"); value != "" {
		var parseErr error
		registerBatch, parseErr = strconv.Atoi(value)
		if parseErr != nil || registerBatch < 1 || registerBatch > 100 {
			t.Fatal("TINYSTORE_REGISTER_BATCH must be in 1..100")
		}
	}
	for _, stage := range []string{"register", "append", "seal"} {
		t.Run(stage, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), fileName)
			if pageSize == 8192 {
				if err := precreatePageSize(t.Context(), path, pageSize); err != nil {
					t.Fatal(err)
				}
			}
			store, openErr := openAt(t, path, Options{Retention: 365 * 24 * time.Hour, MaxHeadSamples: 8192, MaxBatchSamples: 20000, MaxBatchBytes: 64 << 20})
			if openErr != nil {
				t.Fatal(openErr)
			}
			defer store.Close(t.Context())
			if err := store.file.Update(t.Context(), func(tx *sql.Tx) error {
				_, err := tx.ExecContext(t.Context(), `pragma wal_autocheckpoint=0`)
				return err
			}); err != nil {
				t.Fatal(err)
			}
			count := 1000
			if stage == "seal" {
				count = 64
			}
			series := make([]Series, count)
			for id := range series {
				series[id] = Series{Kind: Gauge, Labels: []Label{{Name: "__name__", Value: "physical"}, {Name: "id", Value: fmt.Sprint(id)}}}
			}
			start := time.Now().UnixMilli() - 3600000
			if stage != "register" {
				for first := 0; first < count; first += 64 {
					var batch []Batch
					for id := first; id < min(first+64, count); id++ {
						n := 1
						if stage == "seal" {
							n = 241
						}
						points := make([]Sample, n)
						for index := range points {
							points[index] = Sample{At: start + int64(index)*10000, Value: float64((id + index) % 97)}
						}
						batch = append(batch, Batch{Series: series[id], Samples: points})
					}
					if err := store.Ingest(t.Context(), batch); err != nil {
						t.Fatal(err)
					}
				}
			}
			if err := truncateWAL(t.Context(), path); err != nil {
				t.Fatal(err)
			}
			before, statusErr := store.file.WriterCounters(t.Context())
			if statusErr != nil {
				t.Fatal(statusErr)
			}
			fmt.Fprintf(os.Stderr, "PHYSICAL_BEGIN %s\n", stage)
			begin := time.Now()
			samples, sealed := 0, 0
			switch stage {
			case "register":
				for first := 0; first < count; first += registerBatch {
					var batch []Batch
					for id := first; id < min(first+registerBatch, count); id++ {
						batch = append(batch, Batch{Series: series[id], Samples: []Sample{{At: start, Value: float64(id)}}})
					}
					if err := store.Ingest(t.Context(), batch); err != nil {
						t.Fatal(err)
					}
					samples += len(batch)
				}
			case "append":
				for round := 1; round <= 10; round++ {
					for first := 0; first < count; first += 100 {
						var batch []Batch
						for id := first; id < min(first+100, count); id++ {
							batch = append(batch, Batch{Series: series[id], Samples: []Sample{{At: start + int64(round)*10000, Value: float64((id + round) % 97)}}})
						}
						if err := store.Ingest(t.Context(), batch); err != nil {
							t.Fatal(err)
						}
						samples += len(batch)
					}
				}
			case "seal":
				for {
					result, err := store.Maintain(t.Context())
					if err != nil {
						t.Fatal(err)
					}
					sealed += result.SealedBlocks
					if result.SealedBlocks == 0 {
						break
					}
				}
			}
			elapsed := time.Since(begin)
			fmt.Fprintf(os.Stderr, "PHYSICAL_END %s\n", stage)
			after, statusErr := store.file.WriterCounters(t.Context())
			if statusErr != nil {
				t.Fatal(statusErr)
			}
			frames, actualPageSize, walErr := walFrames(path)
			if walErr != nil {
				t.Fatal(walErr)
			}
			busy, logFrames, checkpointed, checkpointErr := checkpointWAL(t.Context(), path, "PASSIVE")
			if checkpointErr != nil {
				t.Fatal(checkpointErr)
			}
			if err := store.runtime.Close(t.Context()); err != nil {
				t.Fatal(err)
			}
			info, statErr := os.Stat(path)
			if statErr != nil {
				t.Fatal(statErr)
			}
			t.Logf("PHYSICAL stage=%s register_batch=%d samples=%d sealed=%d elapsed=%s commits=%d cache_writes=%d cache_spills=%d cache_hits=%d cache_misses=%d wal_frames=%d page_size=%d wal_bytes=%d checkpoint_busy=%d checkpoint_log=%d checkpointed=%d file_bytes_closed=%d", stage, registerBatch, samples, sealed, elapsed, after.Commits-before.Commits, after.CacheWrites-before.CacheWrites, after.CacheSpills-before.CacheSpills, after.CacheHits-before.CacheHits, after.CacheMisses-before.CacheMisses, frames, actualPageSize, 32+frames*(actualPageSize+24), busy, logFrames, checkpointed, info.Size())
		})
	}
}

func precreatePageSize(ctx context.Context, path string, size int) error {
	db, err := sqlite.OpenDB("file:" + filepath.ToSlash(path))
	if err != nil {
		return err
	}
	defer db.Close()
	if _, err := db.ExecContext(ctx, fmt.Sprintf("pragma page_size=%d", size)); err != nil {
		return err
	}
	if _, err := db.ExecContext(ctx, `vacuum`); err != nil {
		return err
	}
	var actual int
	if err := db.QueryRowContext(ctx, `pragma page_size`).Scan(&actual); err != nil {
		return err
	}
	if actual != size {
		return fmt.Errorf("SQLite page size %d instead of %d", actual, size)
	}
	return nil
}

func truncateWAL(ctx context.Context, path string) error {
	busy, _, _, err := checkpointWAL(ctx, path, "TRUNCATE")
	if err != nil {
		return err
	}
	if busy != 0 {
		return fmt.Errorf("WAL checkpoint busy")
	}
	return nil
}

func checkpointWAL(ctx context.Context, path, mode string) (int, int, int, error) {
	db, err := sqlite.OpenDB("file:" + filepath.ToSlash(path))
	if err != nil {
		return 0, 0, 0, err
	}
	defer db.Close()
	var busy, logFrames, checkpointed int
	if err := db.QueryRowContext(ctx, `pragma wal_checkpoint(`+mode+`)`).Scan(&busy, &logFrames, &checkpointed); err != nil {
		return 0, 0, 0, err
	}
	return busy, logFrames, checkpointed, nil
}

func walFrames(path string) (int64, int64, error) {
	file, openErr := os.Open(path + "-wal")
	if openErr != nil {
		return 0, 0, openErr
	}
	defer file.Close()
	var header [12]byte
	if _, err := file.ReadAt(header[:], 0); err != nil {
		return 0, 0, err
	}
	pageSize := int64(binary.BigEndian.Uint32(header[8:12]))
	if pageSize == 1 {
		pageSize = 65536
	}
	info, err := file.Stat()
	if err != nil {
		return 0, 0, err
	}
	if pageSize < 512 || (info.Size()-32)%(pageSize+24) != 0 {
		return 0, 0, fmt.Errorf("invalid WAL frame size")
	}
	return (info.Size() - 32) / (pageSize + 24), pageSize, nil
}
