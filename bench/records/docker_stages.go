package main

import (
	"context"
	"database/sql"
	"fmt"
	"log"
	"path/filepath"
	"slices"
	"time"

	"github.com/tinyshed/tinystore/records"
)

// measureDocker seals each container's records a full segment at a time, the
// way the research round cut them, then asks what an operator would
func measureDocker(ctx context.Context, dir string, containers []container) {
	first, last, total, skipped := corpusSpan(containers)
	h := openHarness(ctx, dir, first)
	start := time.Now()
	for _, found := range containers {
		for _, batch := range minuteBatches(found.records) {
			h.append(ctx, batch)
			if h.appended%16_384 < len(batch) {
				h.maintain(ctx)
			}
		}
	}
	h.sealAll(ctx, last)
	took := time.Since(start)
	fmt.Printf("stage=docker containers=%d records=%d skipped=%d append_and_seal=%v records_per_second=%.0f\n",
		len(containers), total, skipped, took.Round(time.Millisecond), float64(total)/took.Seconds())

	operatorQueries(ctx, h, containers)
	h.close(ctx)
	reportGroups(ctx, dir, containers)
	reportFile(ctx, dir, total)
}

// reportGroups divides the payload, blocks and segment rows, between the
// containers that write JSON and those that write text, as the research
// round's table did; a container belongs to what most of its lines are
func reportGroups(ctx context.Context, dir string, containers []container) {
	db, err := sql.Open("sqlite", filepath.Join(dir, "records.db"))
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()
	const payload = `select s.name, coalesce((select sum(size) from blocks where stream = s.id), 0)
		+ coalesce((select sum(length(body)) from segments where stream = s.id), 0) from streams s`
	rows, err := db.QueryContext(ctx, payload)
	if err != nil {
		log.Fatal(err)
	}
	defer rows.Close()
	bytes := map[string]int64{}
	for rows.Next() {
		var name string
		var size int64
		if err = rows.Scan(&name, &size); err != nil {
			log.Fatal(err)
		}
		bytes[name] = size
	}
	if err = rows.Err(); err != nil {
		log.Fatal(err)
	}
	groups := map[string][2]int64{}
	for _, found := range containers {
		kind := "text"
		if jsonLines(found.records)*2 > len(found.records) {
			kind = "json"
		}
		group := groups[kind]
		groups[kind] = [2]int64{group[0] + bytes[found.name], group[1] + int64(len(found.records))}
	}
	for kind, group := range groups {
		perRecord := float64(group[0]) / float64(group[1])
		fmt.Printf("group=%s records=%d payload_bytes_per_record=%.4f\n", kind, group[1], perRecord)
	}
}

func jsonLines(found []records.Record) int {
	count := 0
	for _, record := range found {
		if record.Body == nil {
			count++
		}
	}
	return count
}

// replayDocker runs the corpus on its own clock: each minute's records are
// appended together, and maintenance runs every minute, sealing a head that is
// full or an hour old and merging small segments, as a store in production
// would; then it asks what an operator would
func replayDocker(ctx context.Context, dir string, containers []container, sealAge time.Duration) {
	var all []records.Record
	for _, found := range containers {
		all = append(all, found.records...)
	}
	slices.SortStableFunc(all, func(a, b records.Record) int { return a.At.Compare(b.At) })
	h := openHarnessSealing(ctx, dir, all[0].At, sealAge)
	start := time.Now()
	var work records.Maintenance
	for _, batch := range minuteBatches(all) {
		h.setClock(batch[len(batch)-1].At.Truncate(time.Minute).Add(time.Minute))
		h.append(ctx, batch)
		work = sum(work, h.maintain(ctx))
	}
	work = sum(work, h.sealAll(ctx, all[len(all)-1].At.Add(sealAge)))
	took := time.Since(start)
	fmt.Printf("stage=replay seal_age=%v records=%d sealed_segments=%d replay=%v\n", sealAge, len(all),
		work.SealedSegments, took.Round(time.Millisecond))
	fmt.Printf("merged_segments=%d merged_records=%d merged_records_per_record=%.2f\n", work.MergedSegments,
		work.MergedRecords, float64(work.MergedRecords)/float64(len(all)))
	operatorQueries(ctx, h, containers)
	followEverything(ctx, h, len(all))
	h.close(ctx)
	reportFile(ctx, dir, len(all))
}

func corpusSpan(containers []container) (first, last time.Time, total, skipped int) {
	first, last = containers[0].records[0].At, containers[0].records[0].At
	for _, found := range containers {
		total, skipped = total+len(found.records), skipped+found.skipped
		for _, record := range found.records {
			if record.At.Before(first) {
				first = record.At
			}
			if record.At.After(last) {
				last = record.At
			}
		}
	}
	return first, last, total, skipped
}

// minuteBatches cuts records into the batches a handler flushing each minute
// would write: records received within one minute, at most 1024 of them, so
// no record lags its batch by the minute that makes it late
func minuteBatches(all []records.Record) [][]records.Record {
	var batches [][]records.Record
	start := 0
	for i := range all {
		minute := all[start].At.Truncate(time.Minute)
		if i-start == 1024 || !all[i].At.Truncate(time.Minute).Equal(minute) {
			batches, start = append(batches, all[start:i]), i
		}
	}
	return append(batches, all[start:])
}

// operatorQueries asks what the research round asked: the busiest minute, one
// request id over all time, and every error written as a level inside JSON
func operatorQueries(ctx context.Context, h *harness, containers []container) {
	var everything []records.Record
	for _, found := range containers {
		everything = append(everything, found.records...)
	}
	minute := busiestMinute(everything)
	request := requestID(everything)
	queries := []struct {
		name  string
		query records.Query
	}{
		{"busiest minute", records.Query{From: minute, To: minute.Add(time.Minute), Limit: 10_000}},
		{"one requestId value, all time", records.Query{Attrs: []records.Field{request}}},
		{"level = 50 inside JSON, all time", records.Query{Attrs: []records.Field{{Key: "level", Value: "50"}}}},
	}
	for _, q := range queries {
		ask(ctx, h, q.name, q.query, expected(everything, q.query))
	}
}

func busiestMinute(everything []records.Record) time.Time {
	minutes := map[time.Time]int{}
	for _, record := range everything {
		minutes[record.At.Truncate(time.Minute)]++
	}
	busiest := everything[0].At.Truncate(time.Minute)
	for minute, count := range minutes {
		if count > minutes[busiest] || (count == minutes[busiest] && minute.Before(busiest)) {
			busiest = minute
		}
	}
	return busiest
}

// requestID is the research round's choice: the first requestId past the middle
func requestID(everything []records.Record) records.Field {
	for i := len(everything) / 2; i < len(everything); i++ {
		for _, field := range everything[i].Attrs {
			if field.Key == "requestId" {
				return field
			}
		}
	}
	log.Fatal("no requestId in the corpus")
	return records.Field{}
}

func expected(everything []records.Record, query records.Query) int {
	count := 0
	for _, record := range everything {
		inRange := (query.From.IsZero() || !record.At.Before(query.From)) &&
			(query.To.IsZero() || record.At.Before(query.To))
		if inRange && containsAll(record.Attrs, query.Attrs) {
			count++
		}
	}
	return count
}

func containsAll(fields, wanted []records.Field) bool {
	for _, want := range wanted {
		if !slices.Contains(fields, want) {
			return false
		}
	}
	return true
}

// ask pages through a query and reports what it fetched
func ask(ctx context.Context, h *harness, name string, query records.Query, want int) {
	before, start := h.logs.Stats(), time.Now()
	rows, pages := 0, 0
	for {
		page, err := h.logs.Read(ctx, query)
		if err != nil {
			log.Fatal(name, err)
		}
		rows, pages = rows+len(page.Records), pages+1
		if !page.More {
			break
		}
		query = page.Next
	}
	after := h.logs.Stats()
	if rows != want {
		log.Fatalf("%s: %d rows, want %d", name, rows, want)
	}
	fmt.Printf("query=%q rows=%d pages=%d blocks=%d bytes=%d time=%v\n", name, rows, pages,
		after.ReadBlocks-before.ReadBlocks, after.ReadBytes-before.ReadBytes, time.Since(start).Round(time.Microsecond))
}

func sum(a, b records.Maintenance) records.Maintenance {
	a.SealedSegments += b.SealedSegments
	a.MergedSegments += b.MergedSegments
	a.MergedRecords += b.MergedRecords
	return a
}

// followEverything reads every sealed record from the first place, a
// thousand at a time, as a consumer behind by the whole corpus would
func followEverything(ctx context.Context, h *harness, want int) {
	before, start := h.logs.Stats(), time.Now()
	cursor, followed, batches := records.Cursor{}, 0, 0
	for {
		batch, err := h.logs.Follow(ctx, cursor, 1000)
		if err != nil {
			log.Fatal(err)
		}
		if len(batch.Records) == 0 {
			break
		}
		followed, batches, cursor = followed+len(batch.Records), batches+1, batch.Next
	}
	after := h.logs.Stats()
	if followed != want {
		log.Fatalf("followed %d records, want %d", followed, want)
	}
	fmt.Printf("follow=everything records=%d batches=%d blocks=%d bytes=%d time=%v\n", followed, batches,
		after.ReadBlocks-before.ReadBlocks, after.ReadBytes-before.ReadBytes, time.Since(start).Round(time.Millisecond))
}
