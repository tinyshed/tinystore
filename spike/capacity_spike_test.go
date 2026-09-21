package spike

// the capacity model after raw and its block were given the same lifetime

import (
	"database/sql"
	"math/rand/v2"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/klauspost/compress/zstd"
)

const (
	targetSamples = 240
	maxBlockSpan  = 30 * 24 * time.Hour
	retention     = 30 * 24 * time.Hour
	day           = 24 * time.Hour
	epoch         = int64(1_800_000_000_000)
)

type density struct {
	name     string
	interval time.Duration
}

var measuredDensities = []density{
	{name: "15 seconds", interval: 15 * time.Second},
	{name: "1 minute", interval: time.Minute},
	{name: "5 minutes", interval: 5 * time.Minute},
	{name: "1 hour", interval: time.Hour},
	{name: "1 day", interval: day},
}

func TestWhatEachDensityCostsOnDisk(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}

	writer, _ := baselineCodec(t)
	const (
		totalSamples = 2_400_000
		seriesCount  = 1_000
	)

	for _, kind := range []string{"whole numbers", "noisy gauge"} {
		for _, d := range measuredDensities {
			perBlock := samplesPerBlock(d.interval)
			path := filepath.Join(t.TempDir(), "capacity.db")
			db := openBlocks(t, path)
			seedStateRows(t, db, seriesCount)
			payloadBytes, blocks := writeCapacityData(
				t, db, writer, kind, d.interval, totalSamples, seriesCount,
			)
			checkpoint(t, db)
			size := fileBytes(t, path)

			t.Logf("%-13s %-10s %3d samples/block, %6d blocks, span %-9s, payload %5.2f B/sample, sqlite %5.2f B/sample, %.1f MiB",
				kind, d.name, perBlock, blocks, blockSpan(perBlock, d.interval),
				float64(payloadBytes)/totalSamples, float64(size)/totalSamples,
				bytesMiB(size))
			if err := db.Close(); err != nil {
				t.Fatal(err)
			}
		}
	}
}

func TestWhatAMillionDailySeriesCostOnDisk(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}

	path := filepath.Join(t.TempDir(), "telemetry.db")
	db := openBlocks(t, path)
	writer, _ := baselineCodec(t)
	checkpoint(t, db)
	empty := fileBytes(t, path)

	seedStateRows(t, db, seriesTotal)
	checkpoint(t, db)
	withState := fileBytes(t, path)

	const samples = seriesTotal * 30
	payloadBytes, blocks := writeCapacityData(
		t, db, writer, "whole numbers", day, samples, seriesTotal,
	)
	checkpoint(t, db)
	withData := fileBytes(t, path)

	const measuredRegistry = int64(1343 * (1 << 20) / 10)
	t.Logf("series             %d", seriesTotal)
	t.Logf("samples            %d", samples)
	t.Logf("blocks             %d at %d samples/block", blocks, samplesPerBlock(day))
	t.Logf("payload            %.1f MiB (%.2f B/sample)",
		bytesMiB(payloadBytes), float64(payloadBytes)/samples)
	t.Logf("series state       %.1f MiB", bytesMiB(withState-empty))
	t.Logf("blocks + payloads  %.1f MiB", bytesMiB(withData-withState))
	t.Logf("data plane total   %.1f MiB", bytesMiB(withData))
	t.Logf("+ measured registry %.1f MiB", bytesMiB(measuredRegistry))
	t.Logf("projected metrics.db %.1f MiB", bytesMiB(withData+measuredRegistry))

	if err := db.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestWhatHourlyQuietTailClosureWouldCost(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}

	path := filepath.Join(t.TempDir(), "quiet-tail.db")
	db := openBlocks(t, path)
	writer, _ := baselineCodec(t)
	const (
		measuredSeries  = 20_000
		measuredSamples = measuredSeries * 30
	)
	seedStateRows(t, db, measuredSeries)
	payloadBytes, blocks := writeCapacityDataWithBlockSize(
		t, db, writer, "whole numbers", day, measuredSamples, measuredSeries, 1,
	)
	checkpoint(t, db)
	size := fileBytes(t, path)
	perSample := float64(size) / measuredSamples
	const projectedSamples = seriesTotal * 30
	const measuredRegistry = int64(1343 * (1 << 20) / 10)
	projectedData := int64(perSample * projectedSamples)

	t.Logf("measured           %d series, %d samples, %d singleton blocks",
		measuredSeries, measuredSamples, blocks)
	t.Logf("payload            %.2f B/sample", float64(payloadBytes)/measuredSamples)
	t.Logf("sqlite             %.2f B/sample", perSample)
	t.Logf("projected data plane %.1f MiB at one million daily series",
		bytesMiB(projectedData))
	t.Logf("+ measured registry %.1f MiB", bytesMiB(measuredRegistry))
	t.Logf("projected metrics.db %.1f MiB", bytesMiB(projectedData+measuredRegistry))

	if err := db.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestWhatAMonthlySparseHeadCosts(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}

	path := filepath.Join(t.TempDir(), "sparse-head.db")
	db := openBlocks(t, path)
	writer, _ := baselineCodec(t)
	const measuredSeries = 20_000
	seedStateRows(t, db, measuredSeries)
	_, _ = writeCapacityData(
		t, db, writer, "whole numbers", day, measuredSeries*30, measuredSeries,
	)
	checkpoint(t, db)
	packed := fileBytes(t, path)

	writeHeadSamples(t, db, measuredSeries, 30)
	checkpoint(t, db)
	withHead := fileBytes(t, path)
	headBytes := withHead - packed
	const measuredRegistry = int64(1343 * (1 << 20) / 10)
	projectedPacked := int64(float64(packed) / measuredSeries * seriesTotal)
	projectedHead := int64(float64(headBytes) / (measuredSeries * 30) * (seriesTotal * 30))

	t.Logf("measured           %d retained blocks and %d head rows",
		measuredSeries, measuredSeries*30)
	t.Logf("packed state       %.2f B/series", float64(packed)/measuredSeries)
	t.Logf("durable head       %.2f B/sample", float64(headBytes)/(measuredSeries*30))
	t.Logf("projected packed   %.1f MiB", bytesMiB(projectedPacked))
	t.Logf("projected head peak %.1f MiB", bytesMiB(projectedHead))
	t.Logf("+ measured registry %.1f MiB", bytesMiB(measuredRegistry))
	t.Logf("projected metrics.db high-water %.1f MiB",
		bytesMiB(projectedPacked+projectedHead+measuredRegistry))

	if err := db.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestWhetherRetentionChurnPlateaus(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}

	writer, _ := baselineCodec(t)
	for _, kind := range []string{"whole numbers", "noisy gauge"} {
		path := filepath.Join(t.TempDir(), "churn.db")
		db := openBlocks(t, path)
		const seriesCount = 128
		seedStateRows(t, db, seriesCount)

		payload := encode(capacitySamples(kind, targetSamples, 15*time.Second), writer)
		s := summarise(capacitySamples(kind, targetSamples, 15*time.Second))
		nextPayload := int64(1)
		snapshots := make(map[int]capacitySnapshot)
		for currentDay := 1; currentDay <= 90; currentDay++ {
			writeCapacityDay(t, db, payload, s, seriesCount, currentDay-1, &nextPayload)
			cutoff := epoch + int64(time.Duration(currentDay)*day-retention)/int64(time.Millisecond)
			deleteExpiredBlocks(t, db, cutoff)
			checkpoint(t, db)
			snapshots[currentDay] = readCapacitySnapshot(t, db)
		}

		for _, currentDay := range []int{1, 30, 31, 45, 60, 90} {
			snapshot := snapshots[currentDay]
			t.Logf("%-13s day %2d: file %6.1f MiB, live %6.1f MiB, free %6.1f MiB, blocks %d",
				kind, currentDay, bytesMiB(snapshot.physical), bytesMiB(snapshot.live),
				bytesMiB(snapshot.free), snapshot.blocks)
		}

		plateau := snapshots[31].physical
		final := snapshots[90].physical
		if final > plateau+plateau/20 {
			t.Errorf("%s file grew from %.1f MiB after the first delete to %.1f MiB on day 90",
				kind, bytesMiB(plateau), bytesMiB(final))
		}
		if err := db.Close(); err != nil {
			t.Fatal(err)
		}
	}
}

func TestWholeBlockRetentionOvershoot(t *testing.T) {
	for _, d := range measuredDensities {
		perBlock := samplesPerBlock(d.interval)
		span := blockSpan(perBlock, d.interval)
		t.Logf("%-10s %3d samples/block, span %-9s, sample lifetime below %s plus sweep cadence",
			d.name, perBlock, span, retention+span)
	}

	if got := blockSpan(samplesPerBlock(day), day); got != 29*day {
		t.Fatalf("a daily block spans %s instead of 29 days", got)
	}
}

func samplesPerBlock(interval time.Duration) int {
	bySpan := int(maxBlockSpan / interval)
	return min(targetSamples, max(1, bySpan))
}

func blockSpan(samples int, interval time.Duration) time.Duration {
	return time.Duration(samples-1) * interval
}

func capacitySamples(kind string, count int, interval time.Duration) []sample {
	noise := rand.New(rand.NewPCG(1, 2))
	samples := make([]sample, count)
	value := 1000.0
	for i := range samples {
		value += (noise.Float64() - 0.5) * 8
		written := value
		if kind == "whole numbers" {
			written = float64(int64(value))
		}
		samples[i] = sample{
			at:    epoch + int64(time.Duration(i)*interval)/int64(time.Millisecond),
			value: written,
		}
	}
	return samples
}

func seedStateRows(t *testing.T, db *sql.DB, count int) {
	t.Helper()

	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	insert, err := tx.Prepare(
		`insert into series_state(series_id, max_seen_ts, sealed_before) values(?, 0, 0)`,
	)
	if err != nil {
		t.Fatal(err)
	}
	defer insert.Close()
	for id := 1; id <= count; id++ {
		if _, err = insert.Exec(id); err != nil {
			t.Fatal(err)
		}
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
}

func writeCapacityData(t *testing.T, db *sql.DB, writer *zstd.Encoder, kind string,
	interval time.Duration, totalSamples, seriesCount int,
) (payloadBytes int64, blocks int) {
	t.Helper()

	return writeCapacityDataWithBlockSize(
		t, db, writer, kind, interval, totalSamples, seriesCount, samplesPerBlock(interval),
	)
}

func writeCapacityDataWithBlockSize(t *testing.T, db *sql.DB, writer *zstd.Encoder, kind string,
	interval time.Duration, totalSamples, seriesCount, perBlock int,
) (payloadBytes int64, blocks int) {
	t.Helper()

	if totalSamples%perBlock != 0 {
		t.Fatalf("%d samples do not make whole blocks of %d", totalSamples, perBlock)
	}
	blocks = totalSamples / perBlock
	samples := capacitySamples(kind, perBlock, interval)
	packed := encode(samples, writer)
	s := summarise(samples)

	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	putPayload, err := tx.Prepare(`insert into payloads(id, body) values(?, ?)`)
	if err != nil {
		t.Fatal(err)
	}
	defer putPayload.Close()
	putBlock, err := tx.Prepare(
		`insert into blocks(series_id, start_ts, end_ts, count, min, max, sum,
		 first, last, increase, resets, payload_id)
		 values(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
	)
	if err != nil {
		t.Fatal(err)
	}
	defer putBlock.Close()

	nextBlock := time.Duration(perBlock) * interval
	for block := range blocks {
		id := int64(block + 1)
		seriesID := int64(block%seriesCount + 1)
		ordinal := block / seriesCount
		start := epoch + int64(time.Duration(ordinal)*nextBlock)/int64(time.Millisecond)
		end := start + int64(blockSpan(perBlock, interval))/int64(time.Millisecond)
		if _, err = putPayload.Exec(id, packed); err != nil {
			t.Fatal(err)
		}
		if _, err = putBlock.Exec(
			seriesID, start, end, s.count, s.min, s.max, s.sum,
			s.first, s.last, s.increase, s.resets, id,
		); err != nil {
			t.Fatal(err)
		}
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
	return int64(len(packed) * blocks), blocks
}

func writeCapacityDay(t *testing.T, db *sql.DB, payload []byte, s summary,
	seriesCount, dayIndex int, nextPayload *int64,
) {
	t.Helper()

	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	putPayload, err := tx.Prepare(`insert into payloads(id, body) values(?, ?)`)
	if err != nil {
		t.Fatal(err)
	}
	defer putPayload.Close()
	putBlock, err := tx.Prepare(
		`insert into blocks(series_id, start_ts, end_ts, count, min, max, sum,
		 first, last, increase, resets, payload_id)
		 values(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
	)
	if err != nil {
		t.Fatal(err)
	}
	defer putBlock.Close()

	for hour := range 24 {
		start := epoch + int64(time.Duration(dayIndex)*day+time.Duration(hour)*time.Hour)/int64(time.Millisecond)
		end := start + int64(blockSpan(targetSamples, 15*time.Second))/int64(time.Millisecond)
		for seriesID := 1; seriesID <= seriesCount; seriesID++ {
			id := *nextPayload
			*nextPayload++
			if _, err = putPayload.Exec(id, payload); err != nil {
				t.Fatal(err)
			}
			if _, err = putBlock.Exec(
				seriesID, start, end, s.count, s.min, s.max, s.sum,
				s.first, s.last, s.increase, s.resets, id,
			); err != nil {
				t.Fatal(err)
			}
		}
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
}

func writeHeadSamples(t *testing.T, db *sql.DB, seriesCount, samplesPerSeries int) {
	t.Helper()

	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	insert, err := tx.Prepare(`insert into head(series_id, at, value) values(?, ?, ?)`)
	if err != nil {
		t.Fatal(err)
	}
	defer insert.Close()
	for sampleIndex := range samplesPerSeries {
		at := epoch + int64(time.Duration(30+sampleIndex)*day/time.Millisecond)
		for seriesID := 1; seriesID <= seriesCount; seriesID++ {
			if _, err = insert.Exec(seriesID, at, 1000+sampleIndex%17); err != nil {
				t.Fatal(err)
			}
		}
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
}

func deleteExpiredBlocks(t *testing.T, db *sql.DB, cutoff int64) {
	t.Helper()
	if cutoff <= epoch {
		return
	}

	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()
	ids, err := expiredPayloads(tx, cutoff)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = tx.Exec(`delete from blocks where end_ts < ?`, cutoff); err != nil {
		t.Fatal(err)
	}
	removePayload, err := tx.Prepare(`delete from payloads where id = ?`)
	if err != nil {
		t.Fatal(err)
	}
	defer removePayload.Close()
	for _, id := range ids {
		if _, err = removePayload.Exec(id); err != nil {
			t.Fatal(err)
		}
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
}

func expiredPayloads(tx *sql.Tx, cutoff int64) ([]int64, error) {
	rows, err := tx.Query(`select payload_id from blocks where end_ts < ?`, cutoff)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	ids := make([]int64, 0, 4096)
	for rows.Next() {
		var id int64
		if err = rows.Scan(&id); err != nil {
			return nil, err
		}
		ids = append(ids, id)
	}
	return ids, rows.Err()
}

type capacitySnapshot struct {
	physical int64
	live     int64
	free     int64
	blocks   int64
}

func readCapacitySnapshot(t *testing.T, db *sql.DB) capacitySnapshot {
	t.Helper()

	var pageSize, pages, free, blocks int64
	for query, target := range map[string]*int64{
		`pragma page_size`:            &pageSize,
		`pragma page_count`:           &pages,
		`pragma freelist_count`:       &free,
		`select count(*) from blocks`: &blocks,
	} {
		if err := db.QueryRow(query).Scan(target); err != nil {
			t.Fatal(err)
		}
	}
	return capacitySnapshot{
		physical: pages * pageSize,
		live:     (pages - free) * pageSize,
		free:     free * pageSize,
		blocks:   blocks,
	}
}

func fileBytes(t *testing.T, path string) int64 {
	t.Helper()

	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	return info.Size()
}

func bytesMiB(bytes int64) float64 {
	return float64(bytes) / (1 << 20)
}
