package records

import (
	"math"
	"sync"
)

// headBatch is the records one call gives one head, in arrival order
type headBatch struct {
	stream  string
	late    bool
	records []Record
}

// routeToHeads splits a batch by stream, sends what lags more than a minute
// behind the newest record its stream has shown, in the batch or waiting on
// time in its head, to that stream's late head, and cuts each head's share at
// a block's bounds; a record appended alone can be late too:
//
//	web, waiting until 12:00:40; batch 12:00:10 12:00:31 11:50:02 12:00:45
//	→ newest 12:00:45: on time 12:00:10 12:00:31 12:00:45, late 11:50:02
//	web, waiting until 12:00:40; batch 11:59:30 alone        → 70 s behind: late
func routeToHeads(batch []Record, waiting *waitingTimes) []headBatch {
	var routed []headBatch
	for _, records := range byStream(batch) {
		onTime, late := splitLate(records, waiting.reference(records))
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

func splitLate(records []Record, reference int64) (onTime, late []Record) {
	if reference < math.MinInt64+int64(lateness) {
		return records, nil
	}
	for _, record := range records {
		if record.At.UnixNano() < reference-int64(lateness) {
			late = append(late, record)
		} else {
			onTime = append(onTime, record)
		}
	}
	return onTime, late
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

// waitingTimes remembers the newest record each stream's on-time head holds.
// A head that seals empty forgets it, so one record from a wrong clock
// misroutes its stream's records to the late head for one head at most, and
// a restart forgets them all: each batch places its stream until then.
type waitingTimes struct {
	mu     sync.Mutex
	newest map[string]int64
}

// reference is the newest record a stream has shown, in this batch of its
// records or waiting on time in its head
func (w *waitingTimes) reference(records []Record) int64 {
	newest := records[0].At.UnixNano()
	for _, record := range records[1:] {
		newest = max(newest, record.At.UnixNano())
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	if waiting, ok := w.newest[records[0].Stream]; ok {
		return max(newest, waiting)
	}
	return newest
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
