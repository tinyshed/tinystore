package spike

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/tinyshed/tinystore/codec"
)

// a page is also what the write-ahead log writes, so a bigger page buys disk
// with journal volume; both sides use the same batches and the same checkpoints
func TestWhatAPageSizeCostsTheLog(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	blocks, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer blocks.Close()

	const total, perBatch, perBlock = 6000, 100, 240
	for _, class := range []string{"integers", "noisy"} {
		for _, pageSize := range []int{4096, 8192} {
			schema := squeezedSchema("both", true, true, false)
			schema.pageSize = pageSize
			path := filepath.Join(t.TempDir(), "wal.db")
			db := openNight(t, path, schema)
			sparseExec(t, db, `pragma wal_autocheckpoint=0`)

			var logged int64
			for batch := 0; batch < total; batch += perBatch {
				tx, beginErr := db.Begin()
				if beginErr != nil {
					t.Fatal(beginErr)
				}
				for i := batch; i < batch+perBatch; i++ {
					samples := adaptiveSamples(class, perBlock, i)
					old := make([]sample, len(samples))
					for j, s := range samples {
						old[j] = sample{at: s.At, value: s.Value}
					}
					_, packed, encodeErr := blocks.Encode(samples)
					if encodeErr != nil {
						t.Fatal(encodeErr)
					}
					if err = schema.insert(tx, i, int64(i%1000)+1, summarise(old), packed); err != nil {
						t.Fatal(err)
					}
				}
				if err = tx.Commit(); err != nil {
					t.Fatal(err)
				}
				info, statErr := os.Stat(path + "-wal")
				if statErr != nil {
					t.Fatal(statErr)
				}
				logged += info.Size()
				checkpoint(t, db)
			}
			_, file := nightShares(t, db, "payloads")
			samples := float64(total * perBlock)
			t.Logf("%-10s page=%5d  log=%.2f B/sample  file=%.3f B/sample  log is %.1fx the file",
				class, pageSize, float64(logged)/samples, float64(file)/samples,
				float64(logged)/float64(file))
			if err = db.Close(); err != nil {
				t.Fatal(err)
			}
		}
	}
}
