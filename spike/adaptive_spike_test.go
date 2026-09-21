package spike

import (
	"math"
	"math/rand/v2"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore/codec"
)

func TestAdaptiveCodecAgainstTheBaseline(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	c, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	baseline, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1))
	if err != nil {
		t.Fatal(err)
	}
	defer baseline.Close()
	for _, kind := range []string{"constant", "integers", "counter", "temperature", "sine", "noisy", "random bits", "jitter integers"} {
		for _, n := range []int{8, 16, 240} {
			var oldBytes, newBytes int
			var samples []codec.Sample
			for block := range 64 {
				samples = adaptiveSamples(kind, n, block)
				old := make([]sample, n)
				for i, s := range samples {
					old[i] = sample{at: s.At, value: s.Value}
				}
				oldBytes += len(encode(old, baseline))
				head, packed, encodeErr := c.Encode(samples)
				if encodeErr != nil {
					t.Fatal(encodeErr)
				}
				newBytes += len(packed)
				verifyAdaptivePayload(t, c, head, packed, samples)
			}
			const runs = 200
			start := time.Now()
			var packed []byte
			var head codec.Head
			for range runs {
				head, packed, err = c.Encode(samples)
				if err != nil {
					t.Fatal(err)
				}
			}
			encodeTime := time.Since(start) / runs
			start = time.Now()
			for range runs {
				it, decodeErr := c.Decode(head, packed)
				if decodeErr != nil {
					t.Fatal(decodeErr)
				}
				for it.Next() {
					_ = it.Sample()
				}
				if it.Err() != nil {
					t.Fatal(it.Err())
				}
			}
			t.Logf("%-15s n=%3d baseline=%.3f B/sample adaptive=%.3f encode=%s decode=%s",
				kind, n, float64(oldBytes)/float64(64*n), float64(newBytes)/float64(64*n), encodeTime, time.Since(start)/runs)
		}
	}
}

func TestAdaptiveDenseSQLite(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	c, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	baseline, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1))
	if err != nil {
		t.Fatal(err)
	}
	defer baseline.Close()
	for _, kind := range []string{"integers", "counter", "noisy"} {
		for _, mode := range []string{"baseline", "adaptive", "adaptive shared key"} {
			db := openBlocks(t, filepath.Join(t.TempDir(), "adaptive.db"))
			sparseExec(t, db, `pragma foreign_keys=on`)
			shared := mode == "adaptive shared key"
			if shared {
				sparseExec(t, db, `create table shared_blocks(id integer primary key, series_id integer not null,
				 start_ts integer not null, end_ts integer not null, count integer not null,
				 min real, max real, sum real, first real, last real, increase real, resets integer not null,
				 unique(series_id,start_ts)) strict`)
				sparseExec(t, db, `create index shared_expiry on shared_blocks(end_ts)`)
				sparseExec(t, db, `create table shared_payloads(block_id integer primary key references shared_blocks(id) on delete cascade, body blob not null) strict`)
			} else {
				sparseExec(t, db, `create index block_expiry on blocks(end_ts)`)
				sparseExec(t, db, `create index block_payload on blocks(payload_id)`)
			}
			const blocks = 10000
			tx, beginErr := db.Begin()
			if beginErr != nil {
				t.Fatal(beginErr)
			}
			for i := range blocks {
				samples := adaptiveSamples(kind, 240, i)
				old := make([]sample, len(samples))
				for j, s := range samples {
					old[j] = sample{at: s.At, value: s.Value}
				}
				packed := encode(old, baseline)
				if mode != "baseline" {
					_, packed, err = c.Encode(samples)
					if err != nil {
						t.Fatal(err)
					}
				}
				if !shared {
					writeBlock(t, tx, int64(i%1000)+1, summarise(old), packed)
				} else {
					s := summarise(old)
					if _, err = tx.Exec(`insert into shared_blocks values(?,?,?,?,?,?,?,?,?,?,?,?)`,
						i+1, i%1000+1, s.startTS, s.endTS, s.count, s.min, s.max, s.sum, s.first, s.last, s.increase, s.resets); err != nil {
						t.Fatal(err)
					}
					if _, err = tx.Exec(`insert into shared_payloads values(?,?)`, i+1, packed); err != nil {
						t.Fatal(err)
					}
				}
			}
			if err = tx.Commit(); err != nil {
				t.Fatal(err)
			}
			checkpoint(t, db)
			s := readCapacitySnapshot(t, db)
			table := "blocks"
			if shared {
				table = "shared_blocks"
			}
			start := time.Now()
			for range 20 {
				var n int
				var total float64
				if err = db.QueryRow(`select sum(count),sum(sum) from `+table).Scan(&n, &total); err != nil {
					t.Fatal(err)
				}
				if n != blocks*240 {
					t.Fatal("summary lost samples")
				}
			}
			t.Logf("%-10s %-20s sqlite=%.3f B/sample summary=%s", kind, mode, float64(s.physical)/(blocks*240), time.Since(start)/20)
			if shared {
				sparseExec(t, db, `delete from shared_blocks`)
				var remaining int
				if err = db.QueryRow(`select count(*) from shared_payloads`).Scan(&remaining); err != nil || remaining != 0 {
					t.Fatalf("cascade: %d %v", remaining, err)
				}
			}
			if err = db.Close(); err != nil {
				t.Fatal(err)
			}
		}
	}
}

func adaptiveSamples(kind string, n, seed int) []codec.Sample {
	r := rand.New(rand.NewPCG(uint64(seed)+1, 2))
	samples := make([]codec.Sample, n)
	at := epoch + int64(seed)*240*15000
	value := float64(1000 + seed%97)
	for i := range samples {
		switch kind {
		case "constant":
			value = 42
		case "integers", "jitter integers":
			value += float64(r.IntN(9) - 4)
		case "counter":
			value += float64(r.IntN(11))
		case "temperature":
			value = math.Round((20+3*math.Sin(float64(i)/30))*10) / 10
		case "sine":
			value = math.Sin(float64(i) / 30)
		case "noisy":
			value += (r.Float64() - 0.5) * 8
		case "random bits":
			value = math.Float64frombits(r.Uint64())
		}
		at += 15000
		if kind == "jitter integers" {
			at += int64(r.IntN(2001) - 1000)
		}
		samples[i] = codec.Sample{At: at, Value: value}
	}
	return samples
}

func verifyAdaptivePayload(t *testing.T, c *codec.Codec, head codec.Head, payload []byte, want []codec.Sample) {
	t.Helper()
	it, err := c.Decode(head, payload)
	if err != nil {
		t.Fatal(err)
	}
	i := 0
	for it.Next() {
		got := it.Sample()
		if i >= len(want) || got.At != want[i].At || math.Float64bits(got.Value) != math.Float64bits(want[i].Value) {
			t.Fatalf("sample %d changed", i)
		}
		i++
	}
	if it.Err() != nil || i != len(want) {
		t.Fatalf("read %d/%d: %v", i, len(want), it.Err())
	}
}
