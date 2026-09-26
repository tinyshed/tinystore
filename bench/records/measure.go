package main

import (
	"context"
	"database/sql"
	"fmt"
	"log"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"

	_ "modernc.org/sqlite"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/records"
)

// harness is one Manual store and the records engine in it, on a clock the
// stage moves: a head seals by age only when the stage says time has passed
type harness struct {
	dir      string
	runtime  *tinystore.Store
	logs     *records.Store
	mu       sync.Mutex
	now      time.Time
	appended int
}

func openHarness(ctx context.Context, dir string, now time.Time) *harness {
	return openHarnessSealing(ctx, dir, now, time.Hour)
}

// openHarnessSealing opens a harness whose heads seal after sealAge however small
func openHarnessSealing(ctx context.Context, dir string, now time.Time, sealAge time.Duration) *harness {
	h := &harness{dir: dir, now: now}
	runtime, err := tinystore.Open(ctx, dir, tinystore.Options{Manual: true, Clock: h.clock})
	if err != nil {
		log.Fatal(err)
	}
	// ten years either way: a corpus is measured, not expired, and a stage may
	// hold the clock at its first record while it appends the rest
	decade := 10 * 365 * 24 * time.Hour
	logs, err := records.Open(ctx, runtime, records.Options{Retention: decade, ClockSkew: decade, SealAge: sealAge})
	if err != nil {
		log.Fatal(err)
	}
	h.runtime, h.logs = runtime, logs
	return h
}

func (h *harness) clock() time.Time {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.now
}

func (h *harness) setClock(now time.Time) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.now = now
}

// append writes a batch as few Appends as the engine's 4 MiB a call allows
func (h *harness) append(ctx context.Context, batch []records.Record) {
	for start := 0; start < len(batch); {
		end, input := start, 0
		for end < len(batch) && (end == start || input+inputSize(&batch[end]) <= 4<<20) {
			input += inputSize(&batch[end])
			end++
		}
		if err := h.logs.Append(ctx, batch[start:end]...); err != nil {
			log.Fatal(err)
		}
		start = end
	}
	h.appended += len(batch)
}

func (h *harness) maintain(ctx context.Context) records.Maintenance {
	work, err := h.logs.Maintain(ctx)
	if err != nil {
		log.Fatal(err)
	}
	return work
}

// sealAll moves the clock past every head's age, so that what waits seals
func (h *harness) sealAll(ctx context.Context, after time.Time) records.Maintenance {
	h.setClock(after.Add(2 * time.Hour))
	return h.maintain(ctx)
}

func (h *harness) close(ctx context.Context) {
	if err := h.runtime.Close(ctx); err != nil {
		log.Fatal(err)
	}
}

// appendInBatches appends records in arrival order a batch at a time, a
// handler's flush or a research round's segment, sealing every head that fills
func (h *harness) appendInBatches(ctx context.Context, fixture []records.Record, batch int) time.Duration {
	start := time.Now()
	for from := 0; from < len(fixture); from += batch {
		h.append(ctx, fixture[from:min(from+batch, len(fixture))])
		if h.appended%16_384 < batch {
			h.maintain(ctx)
		}
	}
	return time.Since(start)
}

// measureFixture writes a fixture, seals it, reads one second at a hundred
// places, and divides the file
func measureFixture(ctx context.Context, dir string, fixture []records.Record, name string, batch int) {
	end := slices.MaxFunc(fixture, func(a, b records.Record) int { return a.At.Compare(b.At) }).At
	h := openHarness(ctx, dir, end.Add(time.Minute))
	took := h.appendInBatches(ctx, fixture, batch)
	h.sealAll(ctx, end)
	fmt.Printf("stage=%q records=%d batch=%d append_and_seal=%v records_per_second=%.0f\n",
		name, len(fixture), batch, took.Round(time.Millisecond), float64(len(fixture))/took.Seconds())

	secondReads(ctx, h, fixture)
	h.close(ctx)
	reportFile(ctx, dir, len(fixture))
}

// secondReads reads [t, t+1s) at the hundred places the research round read:
// from record 10,000 of a million, every 9,000th
func secondReads(ctx context.Context, h *harness, fixture []records.Record) {
	before := h.logs.Stats()
	rows, start := 0, time.Now()
	for i := range 100 {
		from := fixture[len(fixture)/100+i*(len(fixture)*9/1000)].At
		page, err := h.logs.Read(ctx, records.Query{From: from, To: from.Add(time.Second), Limit: 10_000})
		if err != nil || page.More {
			log.Fatalf("a one-second read: more %v, %v", page.More, err)
		}
		rows += len(page.Records)
	}
	after := h.logs.Stats()
	fmt.Printf("one_second_reads=100 blocks_per_read=%.2f rows_per_read=%.0f bytes_per_read=%.0f time_per_read=%v\n",
		float64(after.ReadBlocks-before.ReadBlocks)/100, float64(rows)/100,
		float64(after.ReadBytes-before.ReadBytes)/100, (time.Since(start) / 100).Round(time.Microsecond))
}

// reportFile vacuums the closed file, then divides it by object: each b-tree's
// pages, what its cells hold and what they leave unused
func reportFile(ctx context.Context, dir string, count int) {
	db, err := sql.Open("sqlite", filepath.Join(dir, "records.db"))
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()
	if _, err = db.ExecContext(ctx, `vacuum`); err != nil {
		log.Fatal(err)
	}
	var size, widest, blocks int64
	query := `select page_count * page_size, (select max(last_at - first_at) from blocks), (select count(*) from blocks)
		from pragma_page_count, pragma_page_size`
	if err = db.QueryRowContext(ctx, query).Scan(&size, &widest, &blocks); err != nil {
		log.Fatal(err)
	}
	fmt.Printf("file_bytes=%d file_bytes_per_record=%.4f blocks=%d widest_block=%v\n",
		size, float64(size)/float64(count), blocks, time.Duration(widest))
	fmt.Printf("segments %s\n", segmentSizes(ctx, db))
	fmt.Printf("objects %s\n", fileObjects(ctx, db, count))
}

// segmentSizes is how many segments hold records and how many they hold: the
// quartiles, and the share of records in segments below a hundred; a segment
// merged into another is a place, and holds none
func segmentSizes(ctx context.Context, db *sql.DB) string {
	var places int
	err := db.QueryRowContext(ctx, `select count(*) from segments where holder is not null`).Scan(&places)
	if err != nil {
		log.Fatal(err)
	}
	return fmt.Sprintf("places=%d %s", places, heldSizes(ctx, db))
}

func heldSizes(ctx context.Context, db *sql.DB) string {
	rows, err := db.QueryContext(ctx, `select held from segments where holder is null order by held`)
	if err != nil {
		log.Fatal(err)
	}
	defer rows.Close()
	var counts []int
	small, total := 0, 0
	for rows.Next() {
		var count int
		if err = rows.Scan(&count); err != nil {
			log.Fatal(err)
		}
		counts, total = append(counts, count), total+count
		if count < 100 {
			small += count
		}
	}
	if err = rows.Err(); err != nil || len(counts) == 0 {
		log.Fatal("no segments ", err)
	}
	quartile := func(q int) int { return counts[(len(counts)-1)*q/4] }
	return fmt.Sprintf("count=%d quartiles=%d/%d/%d/%d/%d records_in_segments_below_100=%.1f%%", len(counts),
		quartile(0), quartile(1), quartile(2), quartile(3), quartile(4), 100*float64(small)/float64(total))
}

func fileObjects(ctx context.Context, db *sql.DB, count int) string {
	rows, err := db.QueryContext(ctx,
		`select name, sum(pgsize), sum(payload), sum(unused) from dbstat group by name order by 2 desc`)
	if err != nil {
		log.Fatal(err)
	}
	defer rows.Close()
	var report []string
	for rows.Next() {
		var name string
		var pages, payload, unused int64
		if err = rows.Scan(&name, &pages, &payload, &unused); err != nil {
			log.Fatal(err)
		}
		report = append(report, fmt.Sprintf("%s=%.4f(%d/%d/%d)", name, float64(pages)/float64(count),
			pages, payload, unused))
	}
	if err = rows.Err(); err != nil {
		log.Fatal(err)
	}
	return strings.Join(report, " ")
}
