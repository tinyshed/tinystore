package server

import (
	"encoding/json"

	"github.com/tinyshed/tinystore/blobs"
	"github.com/tinyshed/tinystore/jobs"
	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/metrics"
	"github.com/tinyshed/tinystore/records"
	"github.com/tinyshed/tinystore/sqldb"
)

// holdAnswer counts an answer an engine has handed over in the store's memory
// until its handler returns: the engine counted it only while it read it, and
// a client takes the DATA at its own pace. A store without Options.Memory is
// not asked, so the answer is not even weighed.
func (c *call) holdAnswer(weigh func() int64) (release func(), err error) {
	store := c.session.server.store
	if store.Memory().Capacity == 0 {
		return func() {}, nil
	}
	reserved, err := store.Reserve(c.ctx, max(weigh(), 1))
	if err != nil {
		return nil, err
	}
	return reserved.Release, nil
}

// an answer weighs its strings and bytes, and two words for every value and
// every item, what Go keeps beside them
const overhead = 16

func weighRows(rows sqldb.Rows) int64 {
	total := int64(0)
	for _, column := range rows.Columns {
		total += int64(len(column)) + overhead
	}
	for _, row := range rows.Values {
		for _, value := range row {
			switch v := value.(type) {
			case string:
				total += int64(len(v))
			case []byte:
				total += int64(len(v))
			}
			total += overhead
		}
	}
	return total
}

func weighRecords(found []records.Record) int64 {
	total := int64(0)
	for _, record := range found {
		total += int64(len(record.Stream)+len(record.Name)) + 4*overhead
		if record.Body != nil {
			total += int64(len(*record.Body))
		}
		for _, fields := range [][]records.Field{record.Context, record.Attrs} {
			for _, field := range fields {
				total += int64(len(field.Key)+len(field.Value)) + overhead
			}
		}
	}
	return total
}

func weighSeries(results []metrics.Result) int64 {
	total := int64(0)
	for _, result := range results {
		total += weighLabels(result.Series.Labels) + int64(len(result.Samples))*overhead
	}
	return total
}

func weighBuckets(results []metrics.AggregateResult) int64 {
	total := int64(0)
	for _, result := range results {
		total += weighLabels(result.Series.Labels) + int64(len(result.Buckets))*3*overhead
	}
	return total
}

func weighLabels(labels []metrics.Label) int64 {
	total := int64(overhead)
	for _, label := range labels {
		total += int64(len(label.Name)+len(label.Value)) + overhead
	}
	return total
}

func weighEntries(entries []kv.Entry[kv.Raw]) int64 {
	total := int64(0)
	for _, entry := range entries {
		total += int64(len(entry.Key)+len(entry.Value.Bytes)) + 4*overhead
	}
	return total
}

func weighObjects(objects []blobs.Object) int64 {
	total := int64(0)
	for _, object := range objects {
		total += int64(len(object.Key)+len(object.ETag)+len(object.ContentType)) + 6*overhead
		for name, value := range object.Meta {
			total += int64(len(name)+len(value)) + overhead
		}
	}
	return total
}

func weighJobs[V any](entries []jobs.Entry[V]) int64 {
	total := int64(0)
	for _, entry := range entries {
		total += int64(len(entry.Key)+len(entry.Err)+len(entry.Repeat)) + 6*overhead
		if value, ok := any(entry.Value).(json.RawMessage); ok {
			total += int64(len(value))
		}
	}
	return total
}
