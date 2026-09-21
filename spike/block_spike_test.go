package spike

// what the block format costs, and whether the invariants hold with a writer, a
// compactor and a reader on one file at once

import (
	"context"
	"database/sql"
	"encoding/binary"
	"fmt"
	"math"
	"math/rand/v2"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/klauspost/compress/zstd"
)

type sample struct {
	at    int64 // unix milliseconds
	value float64
}

// summary is what survives the raw samples it was computed from
type summary struct {
	count                 int
	min, max, sum         float64
	first, last, increase float64
	resets                int
	startTS, endTS        int64
}

// summarise reads the samples in time order, which is the only order a reset is visible in
func summarise(samples []sample) summary {
	if len(samples) == 0 {
		return summary{}
	}

	out := summary{
		count:   len(samples),
		min:     samples[0].value,
		max:     samples[0].value,
		first:   samples[0].value,
		startTS: samples[0].at,
		endTS:   samples[len(samples)-1].at,
	}
	previous := samples[0].value
	for i, s := range samples {
		out.sum += s.value
		out.min = math.Min(out.min, s.value)
		out.max = math.Max(out.max, s.value)
		if i > 0 {
			if s.value < previous {
				out.resets++
				out.increase += s.value
			} else {
				out.increase += s.value - previous
			}
		}
		previous = s.value
	}
	out.last = previous
	return out
}

// encode is the format the decision was measured on: varint deltas over the
// timestamps, raw float bits for the values, and zstd over the whole block
func encode(samples []sample, writer *zstd.Encoder) []byte {
	raw := make([]byte, 0, 10+9*len(samples))
	raw = binary.AppendUvarint(raw, uint64(len(samples)))
	if len(samples) > 0 {
		raw = binary.AppendVarint(raw, samples[0].at)
	}
	previous := int64(0)
	if len(samples) > 0 {
		previous = samples[0].at
	}
	for _, s := range samples[1:] {
		raw = binary.AppendVarint(raw, s.at-previous)
		previous = s.at
	}
	for _, s := range samples {
		raw = binary.LittleEndian.AppendUint64(raw, math.Float64bits(s.value))
	}
	return writer.EncodeAll(raw, nil)
}

func decode(packed []byte, reader *zstd.Decoder) ([]sample, error) {
	raw, err := reader.DecodeAll(packed, nil)
	if err != nil {
		return nil, err
	}

	count, read := binary.Uvarint(raw)
	if read <= 0 {
		return nil, fmt.Errorf("the block does not say how many samples it holds")
	}
	raw = raw[read:]
	if count == 0 {
		return nil, nil
	}

	samples := make([]sample, count)
	at, read := binary.Varint(raw)
	if read <= 0 {
		return nil, fmt.Errorf("the block does not say when it starts")
	}
	raw = raw[read:]
	samples[0].at = at
	for i := 1; i < int(count); i++ {
		delta, read := binary.Varint(raw)
		if read <= 0 {
			return nil, fmt.Errorf("sample %d has no timestamp", i)
		}
		raw = raw[read:]
		at += delta
		samples[i].at = at
	}
	if len(raw) < 8*int(count) {
		return nil, fmt.Errorf("the block holds %d values and promised %d", len(raw)/8, count)
	}
	for i := range samples {
		samples[i].value = math.Float64frombits(binary.LittleEndian.Uint64(raw[8*i:]))
	}
	return samples, nil
}

// the gate this whole spike exists for: a counter that reset inside a block keeps its increase
func TestACounterKeepsItsIncreaseAcrossAReset(t *testing.T) {
	packed := summarise([]sample{
		{at: 0, value: 100},
		{at: 1000, value: 110},
		{at: 2000, value: 5},
		{at: 3000, value: 20},
	})

	if packed.increase != 30 {
		t.Errorf("the increase came out %v, and the counter really rose by 30", packed.increase)
	}
	if packed.resets != 1 {
		t.Errorf("it noticed %d resets", packed.resets)
	}
	if packed.first != 100 || packed.last != 20 {
		t.Errorf("the ends are %v and %v", packed.first, packed.last)
	}
}

// the gate: a sample that arrives after its neighbourhood was sealed does not
// make the sealed summary slightly wrong, it makes it a different number
func TestALateCounterSampleWouldRewriteASealedBlock(t *testing.T) {
	const minute = 60_000

	sealed := summarise([]sample{
		{at: 0, value: 100},
		{at: 10 * minute, value: 110},
		{at: 20 * minute, value: 120},
	})
	truth := summarise([]sample{
		{at: 0, value: 100},
		{at: 10 * minute, value: 110},
		{at: 15 * minute, value: 5}, // the counter restarted, and we heard about it late
		{at: 20 * minute, value: 120},
	})

	if sealed.increase != 20 {
		t.Fatalf("the sealed block says %v", sealed.increase)
	}
	if truth.increase != 130 {
		t.Fatalf("the truth is %v", truth.increase)
	}
	if sealed.resets != 0 || truth.resets != 1 {
		t.Errorf("resets read %d sealed and %d true", sealed.resets, truth.resets)
	}

	// so what may be sealed trails the newest sample the series has shown, and a
	// sample that is late by exactly what it is allowed is still on its way
	const allowedLateness = 5 * minute

	mark := watermark(sealed, allowedLateness)
	if mark != 15*minute {
		t.Fatalf("the watermark is at %d", mark)
	}
	if sealable(15*minute, mark) {
		t.Error("15:00 was sealed while a sample five minutes late was still allowed")
	}
	if !sealable(14*minute, mark) {
		t.Error("14:00 is six minutes behind and was still held open")
	}

	// and an agent that was away for a day moves the mark with its own backlog
	// rather than being late because the server's clock went on without it
	yesterday := summarise([]sample{{at: -24 * 60 * minute, value: 1}})
	if watermark(yesterday, allowedLateness) != -24*60*minute-allowedLateness {
		t.Error("the watermark followed something other than the series")
	}
}

// sealable is strictly before the mark: a sample sitting exactly on it is late by
// exactly what it is allowed to be, and is therefore still coming
func sealable(at, mark int64) bool { return at < mark }

// watermark is what may be packed: it trails the newest sample this series has
// shown rather than the wall clock, so an agent that was away for a day and sent
// its backlog is not late merely because the server moved on
func watermark(seen summary, allowedLateness int64) int64 {
	return seen.endTS - allowedLateness
}

// the gate: a compactor packs the safe prefix and leaves the rest, however much has piled up
func TestOnlyTheSafePrefixIsSealed(t *testing.T) {
	const second = 1000

	samples := make([]sample, 0, 300)
	for i := range 300 {
		samples = append(samples, sample{at: int64(i) * 15 * second, value: float64(i)})
	}

	// the newest is 299, and two samples' worth of lateness is still allowed
	mark := watermark(summarise(samples), 30*second)
	safe := safePrefix(samples, mark)

	// three stay open, not two: the pair inside the window and the one sitting
	// exactly on the mark, which is late by exactly what it is allowed to be
	if len(safe) != 297 {
		t.Errorf("it would have sealed %d of 300", len(safe))
	}
	if len(safe) > 0 && !sealable(safe[len(safe)-1].at, mark) {
		t.Error("the last sealed sample was not safe to seal")
	}
	for _, s := range samples[len(safe):] {
		if sealable(s.at, mark) {
			t.Errorf("the sample at %d was left open although it was safe", s.at)
		}
	}

	// and a series nobody has spoken for yet seals nothing at all
	if left := safePrefix(samples, 0); len(left) != 0 {
		t.Errorf("a watermark of zero sealed %d samples", len(left))
	}
}

// baselineCodec is the unversioned prototype every measurement is compared against
func baselineCodec(t *testing.T) (*zstd.Encoder, *zstd.Decoder) {
	t.Helper()

	writer, err := zstd.NewWriter(nil)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = writer.Close() })

	reader, err := zstd.NewReader(nil)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(reader.Close)
	return writer, reader
}

func TestSamplesSurviveTheCodecExactly(t *testing.T) {
	writer, reader := baselineCodec(t)

	for _, name := range []string{"noisy gauge", "counter", "stable gauge"} {
		t.Run(name, func(t *testing.T) {
			samples := series240(name, 0)
			back, err := decode(encode(samples, writer), reader)
			if err != nil {
				t.Fatal(err)
			}
			if len(back) != len(samples) {
				t.Fatalf("%d samples went in and %d came out", len(samples), len(back))
			}
			for i := range samples {
				if back[i] != samples[i] {
					t.Fatalf("sample %d went in as %+v and came out as %+v", i, samples[i], back[i])
				}
			}
		})
	}
}

// series240 is the shape each kind really has, which is what the codec's cost depends on
func series240(kind string, start int64) []sample {
	noise := rand.New(rand.NewPCG(uint64(start)+1, 2))
	samples := make([]sample, 0, 240)
	value := 1000.0
	for i := range 240 {
		switch kind {
		case "noisy gauge":
			value += (noise.Float64() - 0.5) * 8
		case "counter":
			value += noise.Float64() * 10
		case "stable gauge":
			value = 1000
		}
		samples = append(samples, sample{at: start + int64(i)*15_000, value: value})
	}
	return samples
}

func TestWhatABlockCostsAtEachSize(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}

	writer, err := zstd.NewWriter(nil)
	if err != nil {
		t.Fatal(err)
	}
	defer writer.Close()

	const total = 240 * 128 // the same samples every time, cut up differently
	for _, perBlock := range []int{60, 120, 240, 480, 960, 1920} {
		dir := t.TempDir()
		path := filepath.Join(dir, "blocks.db")
		db := openBlocks(t, path)

		samples := make([]sample, 0, total)
		for block := range total / 240 {
			samples = append(samples, series240("noisy gauge", int64(block)*240*15_000)...)
		}

		payload := 0
		tx, err := db.Begin()
		if err != nil {
			t.Fatal(err)
		}
		for start := 0; start < len(samples); start += perBlock {
			end := min(start+perBlock, len(samples))
			packed := encode(samples[start:end], writer)
			payload += len(packed)
			writeBlock(t, tx, int64(1), summarise(samples[start:end]), packed)
		}
		if err = tx.Commit(); err != nil {
			t.Fatal(err)
		}
		checkpoint(t, db)

		size := fileMiB(t, path) * (1 << 20)
		t.Logf("%5d samples per block: %6.2f B per sample on disk, payload %5.2f B, overhead %3.0f%%, %d blocks",
			perBlock, size/float64(total), float64(payload)/float64(total),
			100*(size-float64(payload))/float64(payload), len(samples)/perBlock)
		db.Close()
	}
}

func openBlocks(t *testing.T, path string) *sql.DB {
	t.Helper()

	dsn := "file:" + path + "?_dqs=0&_defensive=1&_pragma=busy_timeout(5000)" +
		"&_pragma=synchronous(NORMAL)&_pragma=temp_store(MEMORY)&_txlock=immediate" +
		"&_pragma=journal_mode(WAL)"
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		t.Fatal(err)
	}
	db.SetMaxOpenConns(1)
	db.SetConnMaxLifetime(0)

	for _, statement := range []string{
		`create table head (series_id integer not null, at integer not null,
		  value real not null, primary key (series_id, at)) strict, without rowid`,
		`create table progress (id integer primary key, written integer not null) strict`,
		`create table series_state (series_id integer primary key, max_seen_ts integer not null,
		  sealed_before integer not null) strict`,
		`insert into progress(id, written) values(1, 0)`,
		`create table payloads (id integer primary key, body blob not null) strict`,
		`create table blocks (series_id integer not null, start_ts integer not null,
		  end_ts integer not null, count integer not null,
		  min real, max real, sum real, first real, last real, increase real,
		  resets integer not null,
		  payload_id integer references payloads(id) on delete set null,
		  primary key (series_id, start_ts)) strict, without rowid`,
	} {
		if _, err = db.Exec(statement); err != nil {
			t.Fatal(err)
		}
	}
	return db
}

func writeBlock(t *testing.T, tx *sql.Tx, seriesID int64, s summary, packed []byte) {
	t.Helper()

	var payloadID int64
	if err := tx.QueryRow(`insert into payloads(body) values(?) returning id`, packed).
		Scan(&payloadID); err != nil {
		t.Fatal(err)
	}
	if _, err := tx.Exec(
		`insert into blocks(series_id, start_ts, end_ts, count, min, max, sum,
		   first, last, increase, resets, payload_id)
		 values(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		seriesID, s.startTS, s.endTS, s.count, s.min, s.max, s.sum,
		s.first, s.last, s.increase, s.resets, payloadID); err != nil {
		t.Fatal(err)
	}
}

// the gate: a writer, a compactor and a reader at once, and the reader never
// sees a sample twice or misses one that was already written
func TestIngestCompactionAndAReaderAtTheSameTime(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}

	dir := t.TempDir()
	path := filepath.Join(dir, "blocks.db")
	db := openBlocks(t, path)
	defer db.Close()

	reader, err := sql.Open("sqlite",
		"file:"+path+"?_dqs=0&_pragma=busy_timeout(5000)&_query_only=1")
	if err != nil {
		t.Fatal(err)
	}
	defer reader.Close()

	writer, err := zstd.NewWriter(nil)
	if err != nil {
		t.Fatal(err)
	}
	defer writer.Close()

	const (
		seriesCount = 200
		perBlock    = 240
		// two samples of a fifteen-second series, so the rule is exercised rather than stated
		allowedLateness = 30_000
		run             = 10 * time.Second
	)

	seedSeries(t, db, seriesCount)

	var written, compacted, reads atomic.Int64
	done := make(chan struct{})
	problems := make(chan string, 64)
	var watching sync.WaitGroup
	watching.Add(2)

	// the writer: samples arrive as a scrape would deliver them, a round per tick
	go func() {
		defer close(done)

		at := time.Now().UnixMilli()
		deadline := time.Now().Add(run)
		for time.Now().Before(deadline) {
			if failed := writeRound(db, seriesCount, at); failed != nil {
				problems <- failed.Error()
				return
			}
			written.Add(seriesCount)
			at += 15_000
		}
	}()

	// the compactor: whatever is ready leaves the head inside the transaction that packs it
	go func() {
		defer watching.Done()

		for {
			select {
			case <-done:
				compactOnce(t, db, writer, perBlock, allowedLateness, &compacted, problems)
				return
			default:
			}
			compactOnce(t, db, writer, perBlock, allowedLateness, &compacted, problems)
			time.Sleep(20 * time.Millisecond)
		}
	}()

	// the reader: blocks and head from one snapshot, which must never disagree with the writer
	go func() {
		defer watching.Done()

		highest := int64(0)
		for {
			select {
			case <-done:
				return
			default:
			}

			seen, claimed := visible(t, reader, problems)
			reads.Add(1)

			if seen != claimed {
				problems <- fmt.Sprintf("one snapshot holds %d samples and says %d were written",
					seen, claimed)
			}
			if seen < highest {
				problems <- fmt.Sprintf("a reader saw %d after having seen %d", seen, highest)
			}
			highest = max(highest, seen)
		}
	}()

	start := time.Now()
	<-done
	elapsed := time.Since(start)
	watching.Wait()

	close(problems)
	for problem := range problems {
		t.Error(problem)
	}

	var headRows, blockRows, blockSamples int64
	if err = db.QueryRow(`select count(*) from head`).Scan(&headRows); err != nil {
		t.Fatal(err)
	}
	if err = db.QueryRow(`select count(*), coalesce(sum(count), 0) from blocks`).
		Scan(&blockRows, &blockSamples); err != nil {
		t.Fatal(err)
	}

	t.Logf("wrote              %d samples in %s (%.0f samples/s over %d series)",
		written.Load(), elapsed.Round(time.Millisecond),
		float64(written.Load())/elapsed.Seconds(), seriesCount)
	t.Logf("packed             %d samples into %d blocks", blockSamples, blockRows)
	t.Logf("still in the head  %d samples", headRows)
	t.Logf("reads              %d snapshots taken while this happened", reads.Load())
	t.Logf("file               %.1f MiB, wal %.1f MiB", fileMiB(t, path), walMiB(t, path))

	if blockSamples+headRows != written.Load() {
		t.Errorf("%d samples were written and %d are held", written.Load(), blockSamples+headRows)
	}
}

// visible is the invariant itself: what is held and what was written, from one snapshot
func visible(t *testing.T, db *sql.DB, problems chan<- string) (held, claimed int64) {
	t.Helper()

	tx, err := db.BeginTx(context.Background(), &sql.TxOptions{ReadOnly: true})
	if err != nil {
		problems <- "read transaction: " + err.Error()
		return 0, 0
	}
	defer func() { _ = tx.Rollback() }()

	var packed, loose int64
	if err = tx.QueryRow(`select coalesce(sum(count), 0) from blocks`).Scan(&packed); err != nil {
		problems <- "reading blocks: " + err.Error()
		return 0, 0
	}
	if err = tx.QueryRow(`select count(*) from head`).Scan(&loose); err != nil {
		problems <- "reading the head: " + err.Error()
		return 0, 0
	}
	if err = tx.QueryRow(`select written from progress where id = 1`).Scan(&claimed); err != nil {
		problems <- "reading the count: " + err.Error()
		return 0, 0
	}
	return packed + loose, claimed
}

// writeRound is one scrape: every series takes a sample, and the count moves with them
func writeRound(db *sql.DB, seriesCount int, at int64) error {
	tx, err := db.Begin()
	if err != nil {
		return fmt.Errorf("begin: %w", err)
	}
	defer func() { _ = tx.Rollback() }()

	insert, err := tx.Prepare(`insert into head(series_id, at, value) values(?, ?, ?)`)
	if err != nil {
		return fmt.Errorf("insert: %w", err)
	}
	defer insert.Close()

	for series := range seriesCount {
		if _, err = insert.Exec(int64(series)+1, at, 1000+rand.Float64()*500); err != nil {
			return fmt.Errorf("insert: %w", err)
		}
	}
	if _, err = tx.Exec(`update progress set written = written + ? where id = 1`,
		seriesCount); err != nil {
		return fmt.Errorf("counting: %w", err)
	}
	// once, at the end of the batch: an agent's backlog must not start rejecting
	// its own neighbours halfway through
	if _, err = tx.Exec(
		`update series_state set max_seen_ts = ? where max_seen_ts < ?`, at, at); err != nil {
		return fmt.Errorf("moving what has been seen: %w", err)
	}
	if err = tx.Commit(); err != nil {
		return fmt.Errorf("commit: %w", err)
	}
	return nil
}

// safePrefix is what the watermark allows to be sealed: the samples strictly
// before it, and never the tail that something later is still permitted to land in
func safePrefix(samples []sample, mark int64) []sample {
	for i, s := range samples {
		if !sealable(s.at, mark) {
			return samples[:i]
		}
	}
	return samples
}

// compactOnce packs the safe prefix of every series that has enough, and deletes what it packed in the same write
func compactOnce(t *testing.T, db *sql.DB, writer *zstd.Encoder,
	perBlock int, allowedLateness int64, compacted *atomic.Int64, problems chan<- string,
) {
	t.Helper()

	ready, err := readyToPack(db, perBlock)
	if err != nil {
		problems <- "finding what is ready: " + err.Error()
		return
	}

	for _, id := range ready {
		samples, err := headOf(db, id, perBlock)
		if err != nil {
			problems <- "reading the head: " + err.Error()
			return
		}

		var seen int64
		if err = db.QueryRow(`select max_seen_ts from series_state where series_id = ?`,
			id).Scan(&seen); err != nil {
			problems <- "reading the frontier: " + err.Error()
			return
		}

		safe := safePrefix(samples, seen-allowedLateness)
		if len(safe) == 0 {
			continue
		}
		if err = pack(t, db, writer, id, safe); err != nil {
			problems <- err.Error()
			return
		}
		compacted.Add(int64(len(safe)))
	}
}

// seedSeries gives every series a frontier before anything is written against it
func seedSeries(t *testing.T, db *sql.DB, count int) {
	t.Helper()

	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback() }()

	for series := range count {
		if _, err = tx.Exec(
			`insert into series_state(series_id, max_seen_ts, sealed_before) values(?, 0, 0)`,
			int64(series)+1); err != nil {
			t.Fatal(err)
		}
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
}

func readyToPack(db *sql.DB, perBlock int) ([]int64, error) {
	rows, err := db.Query(
		`select series_id from head group by series_id having count(*) >= ?`, perBlock)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	ready := []int64{}
	for rows.Next() {
		var id int64
		if err = rows.Scan(&id); err != nil {
			return nil, err
		}
		ready = append(ready, id)
	}
	return ready, rows.Err()
}

// pack writes the block and clears exactly what went into it, in one transaction
func pack(t *testing.T, db *sql.DB, writer *zstd.Encoder, seriesID int64, samples []sample) error {
	t.Helper()

	tx, err := db.Begin()
	if err != nil {
		return fmt.Errorf("begin: %w", err)
	}
	defer func() { _ = tx.Rollback() }()

	writeBlock(t, tx, seriesID, summarise(samples), encode(samples, writer))

	// by key and never by range: a sample that arrived late inside that range
	// would be deleted without ever reaching a block
	clear, err := tx.Prepare(`delete from head where series_id = ? and at = ?`)
	if err != nil {
		return fmt.Errorf("clearing the head: %w", err)
	}
	defer clear.Close()

	for _, s := range samples {
		if _, err = clear.Exec(seriesID, s.at); err != nil {
			return fmt.Errorf("clearing the head: %w", err)
		}
	}
	if _, err = tx.Exec(
		`update series_state set sealed_before = ? where series_id = ? and sealed_before < ?`,
		samples[len(samples)-1].at+1, seriesID, samples[len(samples)-1].at+1); err != nil {
		return fmt.Errorf("moving the frontier: %w", err)
	}
	if err = tx.Commit(); err != nil {
		return fmt.Errorf("commit: %w", err)
	}
	return nil
}

// headOf answers in time order, because a reset is only visible in it
func headOf(db *sql.DB, seriesID int64, limit int) ([]sample, error) {
	rows, err := db.Query(
		`select at, value from head where series_id = ? order by at limit ?`, seriesID, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	samples := make([]sample, 0, limit)
	for rows.Next() {
		var s sample
		if err = rows.Scan(&s.at, &s.value); err != nil {
			return nil, err
		}
		samples = append(samples, s)
	}
	return samples, rows.Err()
}

func walMiB(t *testing.T, path string) float64 {
	t.Helper()

	info, err := os.Stat(path + "-wal")
	if os.IsNotExist(err) {
		return 0
	}
	if err != nil {
		t.Fatal(err)
	}
	return float64(info.Size()) / (1 << 20)
}
