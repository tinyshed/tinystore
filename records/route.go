package records

import (
	"context"
	"database/sql"
	"fmt"
	"math"
	"sync"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// headBatch is the records one call gives one head, in arrival order
type headBatch struct {
	stream  string
	late    bool
	records []Record
}

// routeToHeads splits a batch by stream, sends what lags more than ten
// seconds behind the newest record its stream showed before it, earlier in
// the batch or waiting on time in its head, or behind the store's clock when
// that is earlier, to that stream's late head, and cuts each head's share at a
// block's bounds. A record appended alone can be late too; a batch in time
// order makes none of its own records late however long it spans; a producer
// ahead of the store's clock does not make its neighbours late:
//
//	clock 12:01, web waiting until 12:00:40; batch 12:00:35 12:00:41 11:50:02 12:00:45
//	→ on time 12:00:35 12:00:41 12:00:45, late 11:50:02
//	clock 12:01, nothing waiting; batch 11:58:30 11:59:10 12:00:45   → all on time
//	clock 12:01, web waiting until 12:00:40; batch 12:00:20 alone    → 20 s behind: late
//	clock 12:00, web waiting until 12:05:00; batch 11:59:55 alone    → 5 s behind the clock: on time
func routeToHeads(batch []Record, waiting *waitingTimes, now int64) []headBatch {
	var routed []headBatch
	for _, records := range byStream(batch) {
		onTime, late := splitLate(records, waiting.newestOf(records[0].Stream), now)
		routed = appendCut(routed, onTime, false)
		routed = appendCut(routed, late, true)
	}
	return routed
}

// byStream groups a batch by stream, streams and records in arrival order
func byStream(batch []Record) [][]Record {
	var order []string
	groups := map[string][]Record{}
	for _, record := range batch {
		if _, seen := groups[record.Stream]; !seen {
			order = append(order, record.Stream)
		}
		groups[record.Stream] = append(groups[record.Stream], record)
	}
	streams := make([][]Record, len(order))
	for i, stream := range order {
		streams[i] = groups[stream]
	}
	return streams
}

// splitLate measures each record against newest, the newest time its stream
// showed before it, which every record found on time moves on
func splitLate(records []Record, newest, now int64) (onTime, late []Record) {
	for _, record := range records {
		at := record.At.UnixNano()
		if isLate(at, min(newest, now)) {
			late = append(late, record)
			continue
		}
		onTime = append(onTime, record)
		newest = max(newest, at)
	}
	return onTime, late
}

// isLate is a time more than lateness behind reference; nothing is behind the
// earliest time there is
func isLate(at, reference int64) bool {
	return reference > math.MinInt64+int64(lateness) && at < reference-int64(lateness)
}

// appendCut cuts one head's records into rows no larger than a block
func appendCut(routed []headBatch, records []Record, late bool) []headBatch {
	for start := 0; start < len(records); {
		end := blockEnd(records, start)
		routed = append(routed, headBatch{stream: records[start].Stream, late: late, records: records[start:end]})
		start = end
	}
	return routed
}

// waitingTimes remembers the newest record each stream's on-time head holds,
// read from the heads when the store opens. A head that seals empty forgets it.
type waitingTimes struct {
	mu     sync.Mutex
	newest map[string]int64
}

const selectWaitingTimes = `select stream, max(last_at) from heads where late = 0 group by stream`

func (w *waitingTimes) load(ctx context.Context, file *sqlite.File, names *streams) error {
	w.newest = map[string]int64{}
	err := file.View(ctx, func(tx *sql.Tx) error {
		rows, err := tx.QueryContext(ctx, selectWaitingTimes) //nolint:rowserrcheck // EachRow checks Err
		if err != nil {
			return err
		}
		return sqlite.EachRow(rows, "waiting times", func(rows *sql.Rows) error {
			var stream, newest int64
			if err := rows.Scan(&stream, &newest); err != nil {
				return err
			}
			w.newest[names.name(stream)] = newest
			return nil
		})
	})
	if err != nil {
		return fmt.Errorf("records: load what the heads hold: %w", err)
	}
	return nil
}

// newestOf is the newest record waiting on time in a stream's head, or the
// earliest time there is when none waits
func (w *waitingTimes) newestOf(stream string) int64 {
	w.mu.Lock()
	defer w.mu.Unlock()
	if waiting, ok := w.newest[stream]; ok {
		return waiting
	}
	return math.MinInt64
}

// remember takes the newest on-time record of every row an append wrote
func (w *waitingTimes) remember(rows []headRow) {
	w.mu.Lock()
	defer w.mu.Unlock()
	for _, row := range rows {
		if newest, ok := w.newest[row.stream]; !row.late && (!ok || row.last > newest) {
			w.newest[row.stream] = row.last
		}
	}
}

func (w *waitingTimes) forget(stream string) {
	w.mu.Lock()
	defer w.mu.Unlock()
	delete(w.newest, stream)
}
