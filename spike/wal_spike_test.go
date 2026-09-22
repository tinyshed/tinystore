package spike

// read the way a dashboard reads rather than as fast as the disk allows, and
// tell a growing log file apart from one that is never checkpointed

import (
	"context"
	"database/sql"
	"fmt"
	"os"
	"slices"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/klauspost/compress/zstd"
)

// watcher is one reader's habit: how often it looks and how long it holds the snapshot
type watcher struct {
	name  string
	every time.Duration
	holds time.Duration
	taken []time.Duration
}

func TestTheLogUnderARealisticReadLoad(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}

	dir := t.TempDir()
	path := dir + "/blocks.db"
	db := openBlocks(t, path)
	defer db.Close()

	reader, err := sql.Open("sqlite",
		"file:"+path+"?_dqs=0&_pragma=busy_timeout(5000)&_query_only=1")
	if err != nil {
		t.Fatal(err)
	}
	defer reader.Close()
	reader.SetMaxOpenConns(8)

	packer, err := zstd.NewWriter(nil)
	if err != nil {
		t.Fatal(err)
	}
	defer packer.Close()

	const (
		seriesCount = 200
		perBlock    = 240
		// two samples of a fifteen-second series, so the rule is exercised rather than stated
		allowedLateness = 30_000
		run             = 40 * time.Second
		// the long reader arrives once the log has something in it and leaves well before the end
		longStart = 10 * time.Second
		longHolds = 20 * time.Second
	)

	seedSeries(t, db, seriesCount)

	var written, compacted atomic.Int64
	var rounds []time.Duration
	var roundsMutex sync.Mutex
	done := make(chan struct{})
	problems := make(chan string, 64)
	var working sync.WaitGroup

	working.Go(func() {
		defer close(done)

		at := time.Now().UnixMilli()
		deadline := time.Now().Add(run)
		for time.Now().Before(deadline) {
			start := time.Now()
			if failed := writeRound(db, seriesCount, at); failed != nil {
				problems <- failed.Error()
				return
			}
			roundsMutex.Lock()
			rounds = append(rounds, time.Since(start))
			roundsMutex.Unlock()

			written.Add(seriesCount)
			at += 15_000
			// a scrape does not arrive as fast as the disk allows
			time.Sleep(50 * time.Millisecond)
		}
	})

	working.Go(func() {
		for {
			select {
			case <-done:
				compactOnce(t, db, packer, perBlock, allowedLateness, &compacted, problems)
				return
			default:
			}
			compactOnce(t, db, packer, perBlock, allowedLateness, &compacted, problems)
			time.Sleep(200 * time.Millisecond)
		}
	})

	watchers := []*watcher{
		{name: "every second", every: time.Second, holds: 0},
		{name: "every five seconds", every: 5 * time.Second, holds: 0},
		{name: "heavy, holds half a second", every: 2 * time.Second, holds: 500 * time.Millisecond},
	}
	for _, w := range watchers {
		working.Go(func() {
			ticker := time.NewTicker(w.every)
			defer ticker.Stop()
			for {
				select {
				case <-done:
					return
				case <-ticker.C:
					start := time.Now()
					read(t, reader, w.holds, problems)
					w.taken = append(w.taken, time.Since(start))
				}
			}
		})
	}

	// the reader everybody warns about: one snapshot held open for twenty seconds
	working.Go(func() {
		time.Sleep(longStart)
		tx, opened := reader.BeginTx(context.Background(), &sql.TxOptions{ReadOnly: true})
		if opened != nil {
			problems <- "the long reader: " + opened.Error()
			return
		}
		var held int64
		if scanned := tx.QueryRow(
			`select coalesce(sum(count), 0) from blocks`).Scan(&held); scanned != nil {
			problems <- "the long reader: " + scanned.Error()
		}
		time.Sleep(longHolds)
		_ = tx.Rollback()
	})

	// the observer: the log's size over time, without asking for a checkpoint
	pages := pageSize(t, db)
	type mark struct {
		at    time.Duration
		bytes float64
	}
	marks := []mark{}
	working.Go(func() {
		start := time.Now()
		ticker := time.NewTicker(2 * time.Second)
		defer ticker.Stop()
		for {
			select {
			case <-done:
				return
			case <-ticker.C:
				marks = append(marks, mark{at: time.Since(start), bytes: walBytes(t, path)})
			}
		}
	})

	working.Wait()
	close(problems)
	for problem := range problems {
		t.Error(problem)
	}

	t.Logf("wrote              %d samples, packed %d", written.Load(), compacted.Load())
	for _, m := range marks {
		t.Logf("  at %5s          log %7.1f MiB, %6.0f frames%s",
			m.at.Round(time.Second), m.bytes/(1<<20), m.bytes/float64(pages+24),
			during(m.at, longStart, longHolds))
	}

	var busy, inLog, checkpointed int64
	if err = db.QueryRow(`pragma wal_checkpoint(passive)`).Scan(&busy, &inLog, &checkpointed); err != nil {
		t.Fatal(err)
	}
	t.Logf("passive checkpoint busy %d, %d frames in the log, %d of them copied",
		busy, inLog, checkpointed)
	t.Logf("log after it       %.1f MiB", walBytes(t, path)/(1<<20))

	if err = db.QueryRow(`pragma wal_checkpoint(truncate)`).Scan(&busy, &inLog, &checkpointed); err != nil {
		t.Fatal(err)
	}
	t.Logf("truncate           busy %d, log now %.1f MiB, file %.1f MiB",
		busy, walBytes(t, path)/(1<<20), fileMiB(t, path))

	roundsMutex.Lock()
	t.Logf("a write round      %s", spread(rounds))
	roundsMutex.Unlock()
	for _, w := range watchers {
		t.Logf("%-26s %s over %d reads", w.name, spread(w.taken), len(w.taken))
	}
}

// during says whether a mark was taken while the long reader held its snapshot
func during(at, start, holds time.Duration) string {
	if at >= start && at <= start+holds {
		return "   (a reader is holding one open)"
	}
	return ""
}

// read is one panel's worth of work: the summaries it needs and the head beside them
func read(t *testing.T, db *sql.DB, holds time.Duration, problems chan<- string) {
	t.Helper()

	tx, err := db.BeginTx(context.Background(), &sql.TxOptions{ReadOnly: true})
	if err != nil {
		problems <- "read transaction: " + err.Error()
		return
	}
	defer func() { _ = tx.Rollback() }()

	var packed, loose int64
	if err = tx.QueryRow(
		`select coalesce(sum(count), 0) from blocks where series_id <= 20`).Scan(&packed); err != nil {
		problems <- "reading blocks: " + err.Error()
		return
	}
	if err = tx.QueryRow(
		`select count(*) from head where series_id <= 20`).Scan(&loose); err != nil {
		problems <- "reading the head: " + err.Error()
		return
	}
	if holds > 0 {
		time.Sleep(holds)
	}
}

func pageSize(t *testing.T, db *sql.DB) int64 {
	t.Helper()

	var size int64
	if err := db.QueryRow(`pragma page_size`).Scan(&size); err != nil {
		t.Fatal(err)
	}
	return size
}

func walBytes(t *testing.T, path string) float64 {
	t.Helper()

	info, err := os.Stat(path + "-wal")
	if os.IsNotExist(err) {
		return 0
	}
	if err != nil {
		t.Fatal(err)
	}
	return float64(info.Size())
}

// spread is what a percentile says and an average hides
func spread(taken []time.Duration) string {
	if len(taken) == 0 {
		return "never ran"
	}

	sorted := slices.Clone(taken)
	slices.Sort(sorted)
	at := func(p float64) time.Duration {
		i := int(p * float64(len(sorted)-1))
		return sorted[i].Round(time.Microsecond)
	}
	return fmt.Sprintf("p50 %s, p95 %s, p99 %s, worst %s",
		at(0.50), at(0.95), at(0.99), sorted[len(sorted)-1].Round(time.Microsecond))
}
