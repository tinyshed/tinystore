package records

import (
	"math"
	"slices"
)

// headBatch is the records one call gives one head, in arrival order
type headBatch struct {
	stream  string
	late    bool
	records []Record
}

// routeToHeads splits a batch by stream, sends what lags more than a minute
// behind the median of its stream's records to that stream's late head, and
// cuts each head's share at a block's bounds:
//
//	stream web, median 12:00:31
//	12:00:10  12:00:31  11:50:02  12:00:45   → on time 12:00:10 12:00:31 12:00:45
//	                                           late    11:50:02
func routeToHeads(batch []Record) []headBatch {
	var routed []headBatch
	for _, records := range byStream(batch) {
		onTime, late := splitLate(records)
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

func splitLate(records []Record) (onTime, late []Record) {
	times := make([]int64, len(records))
	for i := range records {
		times[i] = records[i].At.UnixNano()
	}
	slices.Sort(times)
	median := times[len(times)/2]
	if median < math.MinInt64+int64(lateness) {
		return records, nil
	}
	for _, record := range records {
		if record.At.UnixNano() < median-int64(lateness) {
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
