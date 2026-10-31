// Package limits holds the bounds one call meets, each by the name a limit
// error gives it: the same table as Rust's tinystore::limits and the other
// SDKs'. A program cuts its batches to a bound before it calls, and tells
// which bound a limit error met without reading its text:
//
//	for batch := range slices.Chunk(courses, int(limits.SetManyDocuments.Bound)) { … }
//
//	var e *tinystore.Error
//	if errors.As(err, &e) && e.Limit == limits.RequestBytes.Name { … }
//
// A bound the store's options set, its memory ("store memory") or its
// MaxSeries ("series in a store"), is named the same way and is not here.
package limits

// Limit is a bound by its name, as an Error's Limit says it, and the most it
// takes, as its Bound says it.
type Limit struct {
	Name  string
	Bound int64
}

var (
	// ValueBytes is a bucket's value, as it is stored.
	ValueBytes = Limit{"value bytes", 1 << 20}
	// SettingBytes is a live setting's change, as JSON.
	SettingBytes = Limit{"setting bytes", 16 << 10}
	// ConfigBytes is every change a config keeps, together.
	ConfigBytes = Limit{"config bytes", 256 << 10}
	// LockWaiters is the callers that wait for one lock or semaphore at once.
	LockWaiters = Limit{"lock waiters", 10_000}
	// Subscriptions is the subscriptions to the store's channels at once.
	Subscriptions = Limit{"subscriptions", 10_000}
	// JobBytes is a job's payload, its result or a step's answer, as JSON.
	JobBytes = Limit{"job bytes", 1 << 20}
	// ProgressBytes is a run's progress, as JSON.
	ProgressBytes = Limit{"progress bytes", 4 << 10}
	// RowsBytes is the values one call reads from a database: past them, read
	// a page at a time.
	RowsBytes = Limit{"bytes of a call's rows", 64 << 20}
	// EventBytes is one event, its name, fields and text together.
	EventBytes = Limit{"event bytes", 256 << 10}
	// AppendBytes is the events of one append together.
	AppendBytes = Limit{"append bytes", 4 << 20}
	// AggregateRows is the rows one Aggregate answers.
	AggregateRows = Limit{"aggregate rows", 10_000}
	// CountBytes is the different values one Top or Distinct holds, as JSON.
	CountBytes = Limit{"count bytes", 64 << 20}
	// IngestSamples is the samples of one Ingest.
	IngestSamples = Limit{"ingest samples", 10_000}
	// IngestBytes is the bytes of one Ingest's samples, each with its labels.
	IngestBytes = Limit{"ingest bytes", 4 << 20}
	// TextSamples is the samples of one exporter's text.
	TextSamples = Limit{"text samples", 100_000}
	// TextBytes is one exporter's text, and the bytes of its samples.
	TextBytes = Limit{"text bytes", 32 << 20}
	// QuerySeries is the series one query finds.
	QuerySeries = Limit{"series", 1000}
	// QueryBytes is what one query reads from the file.
	QueryBytes = Limit{"bytes read", 16 << 20}
	// QuerySamples is the samples one query decodes.
	QuerySamples = Limit{"samples decoded", 1 << 20}
	// QueryAnswer is the samples or buckets one query answers.
	QueryAnswer = Limit{"answer", 100_000}
	// SetManyDocuments is the documents of one SetMany.
	SetManyDocuments = Limit{"set_many documents", 1000}
	// SetManyBytes is the documents of one SetMany as JSON; one request body,
	// RequestBytes, bounds it first.
	SetManyBytes = Limit{"set_many bytes", 16 << 20}
	// DocumentBytes is one document as JSON, or a table's row as its index
	// reads it.
	DocumentBytes = Limit{"document bytes", 1 << 20}
	// SearchHits is the hits a search reaches, its offset and its limit together.
	SearchHits = Limit{"search hits", 1000}
	// SearchText is a search's text, in bytes.
	SearchText = Limit{"search text", 1 << 10}
	// FilterConditions is the conditions of one search's filter.
	FilterConditions = Limit{"filter conditions", 8}
	// FilterValues is the values the lists of one search's conditions hold together.
	FilterValues = Limit{"filter values", 100}
	// FilterText is a text a filter keeps or asks for, in bytes.
	FilterText = Limit{"filter text", 256}
	// VectorDimensions is a vector's dimensions.
	VectorDimensions = Limit{"vector dimensions", 4096}
	// RequestBytes is one request, its message as the SDK sends it: a larger
	// one fails before it is sent.
	RequestBytes = Limit{"request bytes", 2 << 20}
	// AnswerBytes is one answer, its message as the store sends it.
	AnswerBytes = Limit{"answer bytes", 2 << 20}
)

// all is every bound above, which testdata/wire/limits.json holds.
var all = []Limit{
	ValueBytes, SettingBytes, ConfigBytes, LockWaiters, Subscriptions, JobBytes, ProgressBytes, RowsBytes,
	EventBytes, AppendBytes, AggregateRows, CountBytes, IngestSamples, IngestBytes, TextSamples, TextBytes,
	QuerySeries, QueryBytes, QuerySamples, QueryAnswer, SetManyDocuments, SetManyBytes, DocumentBytes,
	SearchHits, SearchText, FilterConditions, FilterValues, FilterText, VectorDimensions, RequestBytes,
	AnswerBytes,
}
