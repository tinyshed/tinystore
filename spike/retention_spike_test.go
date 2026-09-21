package spike

// what is still answerable after retention removes a block's raw samples

import (
	"database/sql"
	"encoding/binary"
	"math"
	"os"
	"path/filepath"
	"testing"

	"github.com/klauspost/compress/zstd"
)

const fiveMinutes = int64(5 * 60 * 1000)

func TestAPartialRangeNeedsTheRawEdges(t *testing.T) {
	samples := make([]sample, 0, 60)
	for minute := range 60 {
		samples = append(samples, sample{at: int64(minute) * 60_000, value: float64(minute)})
	}

	wanted := samples[17:43]
	whole := summarise(samples)
	exact := summarise(wanted)
	fiveMinute := summarise(samples[15:45])

	if whole.sum == exact.sum {
		t.Fatal("the whole-block summary unexpectedly answered a range inside it")
	}
	if fiveMinute.sum == exact.sum {
		t.Fatal("five-minute buckets unexpectedly answered boundaries at 17 and 43 minutes")
	}
	if exact.startTS != 17*60_000 || exact.endTS != 42*60_000 {
		t.Fatalf("the exact range moved to %d..%d", exact.startTS, exact.endTS)
	}
}

func TestWhatRetentionChoicesCost(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}

	writer, _ := baselineCodec(t)
	const blocks = 128
	for _, kind := range []string{"stable gauge", "whole numbers", "noisy gauge", "counter"} {
		rawBytes := 0
		rollupBytes := 0
		for block := range blocks {
			samples := shaped(kind, int64(block)*240*15_000)
			rawBytes += len(encode(samples, writer))
			rollupBytes += len(encodeRollup(fiveMinuteBuckets(samples), writer))
		}

		samples := float64(blocks * 240)
		rawPerSample := float64(rawBytes) / samples
		rollupPerSample := float64(rollupBytes) / samples
		tieredPerSample := (7*rawPerSample + 23*rollupPerSample) / 30
		t.Logf("%-14s raw %5.2f B/sample, 5m %5.2f B/sample (%4.0f%%), 7d+23d %5.2f B/sample, 30M payload %.1f/%.1f MiB",
			kind, rawPerSample, rollupPerSample, 100*rollupPerSample/rawPerSample,
			tieredPerSample, rawPerSample*30_000_000/(1<<20), tieredPerSample*30_000_000/(1<<20))
	}

	for _, interval := range []int64{15_000, 60_000, fiveMinutes, 24 * 60 * 60 * 1000} {
		count := 240
		if interval == 24*60*60*1000 {
			count = 30
		}
		samples := regularWholeNumbers(count, interval)
		rawBytes := len(encode(samples, writer))
		rollupBytes := len(encodeRollup(fiveMinuteBuckets(samples), writer))
		t.Logf("one sample each %-9s: %3d samples, raw %5d B, 5m %5d B (%4.0f%%)",
			intervalLabel(interval), count, rawBytes, rollupBytes, 100*float64(rollupBytes)/float64(rawBytes))
	}
}

func TestWhatRetentionChoicesCostOnDisk(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}

	writer, _ := baselineCodec(t)
	const (
		seriesCount = 16
		days        = 30
		blocksADay  = 24
		perBlock    = 240
	)
	totalSamples := float64(seriesCount * days * blocksADay * perBlock)

	for _, kind := range []string{"whole numbers", "noisy gauge"} {
		for _, policy := range []string{"raw", "7d raw + 23d 5m", "summary"} {
			path := filepath.Join(t.TempDir(), "retention.db")
			db := openRetention(t, path)
			writeRetention(t, db, writer, kind, policy, seriesCount, days, blocksADay)
			checkpoint(t, db)
			size := fileMiB(t, path) * (1 << 20)
			t.Logf("%-13s %-17s %6.2f B/sample, %5.1f MiB measured, %5.1f MiB at 30M samples",
				kind, policy, size/totalSamples, size/(1<<20), size/totalSamples*30_000_000/(1<<20))
			if err := db.Close(); err != nil {
				t.Fatal(err)
			}
		}
	}
}

func fiveMinuteBuckets(samples []sample) []summary {
	if len(samples) == 0 {
		return nil
	}

	buckets := make([]summary, 0, 1+(samples[len(samples)-1].at-samples[0].at)/fiveMinutes)
	start := 0
	current := samples[0].at / fiveMinutes
	for i := 1; i <= len(samples); i++ {
		if i < len(samples) && samples[i].at/fiveMinutes == current {
			continue
		}
		buckets = append(buckets, summarise(samples[start:i]))
		if i < len(samples) {
			start = i
			current = samples[i].at / fiveMinutes
		}
	}
	return buckets
}

func encodeRollup(buckets []summary, writer *zstd.Encoder) []byte {
	raw := binary.AppendUvarint(nil, uint64(len(buckets)))
	previous := int64(0)
	for i, bucket := range buckets {
		index := bucket.startTS / fiveMinutes
		delta := index
		if i > 0 {
			delta -= previous
		}
		raw = binary.AppendVarint(raw, delta)
		raw = binary.AppendUvarint(raw, uint64(bucket.count))
		for _, value := range []float64{
			bucket.min, bucket.max, bucket.sum, bucket.first, bucket.last, bucket.increase,
		} {
			raw = binary.LittleEndian.AppendUint64(raw, math.Float64bits(value))
		}
		raw = binary.AppendUvarint(raw, uint64(bucket.resets))
		previous = index
	}
	return writer.EncodeAll(raw, nil)
}

func regularWholeNumbers(count int, interval int64) []sample {
	samples := make([]sample, count)
	for i := range samples {
		samples[i] = sample{at: int64(i) * interval, value: float64(1000 + i%17)}
	}
	return samples
}

func intervalLabel(interval int64) string {
	switch interval {
	case 15_000:
		return "15 seconds"
	case 60_000:
		return "minute"
	case fiveMinutes:
		return "5 minutes"
	default:
		return "day"
	}
}

func openRetention(t *testing.T, path string) *sql.DB {
	t.Helper()

	db := openBlocks(t, path)
	if _, err := db.Exec(
		`alter table blocks add column rollup_id integer references payloads(id) on delete set null`,
	); err != nil {
		t.Fatal(err)
	}
	return db
}

func writeRetention(t *testing.T, db *sql.DB, writer *zstd.Encoder, kind, policy string,
	seriesCount, days, blocksADay int,
) {
	t.Helper()

	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()

	for series := range seriesCount {
		for day := range days {
			for block := range blocksADay {
				start := int64((day*blocksADay+block)*240) * 15_000
				samples := shaped(kind, start)
				var raw, rollup []byte
				switch policy {
				case "raw":
					raw = encode(samples, writer)
				case "7d raw + 23d 5m":
					if day >= days-7 {
						raw = encode(samples, writer)
					} else {
						rollup = encodeRollup(fiveMinuteBuckets(samples), writer)
					}
				case "summary":
				default:
					t.Fatalf("unknown retention policy %q", policy)
				}
				writeRetentionBlock(t, tx, int64(series)+1, summarise(samples), raw, rollup)
			}
		}
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
}

func writeRetentionBlock(t *testing.T, tx *sql.Tx, seriesID int64, s summary, raw, rollup []byte) {
	t.Helper()

	insertPayload := func(body []byte) any {
		if body == nil {
			return nil
		}
		var id int64
		if err := tx.QueryRow(`insert into payloads(body) values(?) returning id`, body).Scan(&id); err != nil {
			t.Fatal(err)
		}
		return id
	}
	if _, err := tx.Exec(
		`insert into blocks(series_id, start_ts, end_ts, count, min, max, sum,
		   first, last, increase, resets, payload_id, rollup_id)
		 values(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		seriesID, s.startTS, s.endTS, s.count, s.min, s.max, s.sum,
		s.first, s.last, s.increase, s.resets, insertPayload(raw), insertPayload(rollup)); err != nil {
		t.Fatal(err)
	}
}
