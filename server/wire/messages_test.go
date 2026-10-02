package wire_test

import (
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"math"
	"os"
	"slices"
	"strconv"
	"strings"
	"testing"

	"github.com/tinyshed/tinystore/server/wire"
)

// testdata/messages.json holds every message of docs/wire.md: its bytes as the
// Go types write them and its fields by the names wire.md gives them, which an
// SDK reads and writes in one loop, with the methods' numbers and the errors'
// codes beside them.
//
// The fields are read from the bytes through a schema, the name and kind of
// each message's keys, so a key the Go types write and the schema does not
// name fails here, and every field the schema names is in a vector.
// TINYSTORE_WRITE_VECTORS=1 writes the file anew.
const messagesFile = "testdata/messages.json"

// field is one key of a message. Its kind says how its value reads:
//
//	uint int float bool str bin    as MessagePack types them
//	text                           a str, or a bin whose bytes are not UTF-8
//	key                            a str, a bin, or an integer by its decimal spelling
//	kv value                       nil, an integer or a bin
//	sql value                      nil, a bool, an integer, a float, a str or a bin
//	names, sql names               a map of names to text, or to sql values
//	ints, floats                   a bin of eight-byte little-endian values
//	error?                         nil, or an error
//	[]kind                         an array of kind
//	a message's name               that message
type field struct {
	key  uint64
	name string
	kind string
}

var schema = map[string][]field{
	"hello": {
		{1, "protocol", "uint"},
		{2, "client", "str"},
		{3, "token", "str"},
		{4, "max body", "uint"},
		{5, "stream credit", "uint"},
		{6, "challenge", "bin"},
	},
	"welcome": {
		{1, "protocol", "uint"},
		{2, "server", "str"},
		{3, "instance", "bin"},
		{4, "capability", "str"},
		{5, "max body", "uint"},
		{6, "in flight", "uint"},
		{7, "connection credit", "uint"},
		{8, "stream credit", "uint"},
		{9, "engines", "[]str"},
		{10, "now", "int"},
		{11, "proof", "bin"},
	},
	"goaway": {{1, "code", "str"}, {2, "message", "str"}},
	"error":  {{1, "code", "str"}, {2, "message", "str"}, {3, "what", "names"}},
	"handle": {{1, "handle", "uint"}},
	"empty":  {},

	"kv.bucket": {
		{1, "name", "str"},
		{2, "counters", "bool"},
		{3, "default ttl", "uint"},
		{4, "sliding", "uint"},
		{5, "lose at most", "uint"},
	},
	"kv.call":      kvCall,
	"kv.operation": append([]field{{0, "method", "uint"}}, kvCall...),
	"kv.calls":     {{1, "calls", "[]kv.operation"}},
	"kv.entry": {
		{1, "found", "bool"},
		{2, "value", "kv value"},
		{3, "version", "bin"},
		{4, "expires", "int"},
		{5, "key", "key"},
	},
	"kv.results": {{1, "entries", "[]kv.entry"}},
	"kv.page":    {{1, "more", "bool"}, {2, "after", "key"}},

	"jobs.queue": {
		{1, "name", "str"},
		{2, "lease", "uint"},
		{3, "max attempts", "uint"},
		{4, "backoff first", "uint"},
		{5, "backoff most", "uint"},
		{6, "max waiting", "uint"},
		{7, "keep failed", "uint"},
		{8, "keep done", "uint"},
		{9, "schedule", "jobs.repeat"},
	},
	"jobs.repeat": {{1, "cron", "str"}, {2, "zone", "str"}, {3, "every", "uint"}},
	"jobs.job":    jobsJob,
	"jobs.batch":  {{1, "handle", "uint"}, {2, "jobs", "[]jobs.job"}},
	"jobs.change": append(slices.Clone(jobsJob), field{6, "handle", "uint"}),
	"jobs.key":    {{1, "handle", "uint"}, {2, "key", "str"}},
	"jobs.entry": {
		{1, "found", "bool"},
		{2, "key", "str"},
		{3, "value", "str"},
		{4, "at", "int"},
		{5, "attempt", "uint"},
		{6, "state", "uint"},
		{7, "err", "str"},
		{8, "repeat", "str"},
	},
	"jobs.lease": {{1, "handle", "uint"}, {2, "lease", "uint"}},
	"jobs.held": {
		{1, "found", "bool"},
		{2, "job", "uint"},
		{3, "key", "str"},
		{4, "value", "str"},
		{5, "at", "int"},
		{6, "attempt", "uint"},
	},
	"jobs.outcome": {
		{1, "job", "uint"}, {2, "how", "uint"}, {3, "err", "str"}, {4, "at", "int"}, {5, "after", "uint"},
	},
	"jobs.outcomes": {{1, "outcomes", "[]jobs.outcome"}},
	"jobs.settled":  {{1, "settled", "[]error?"}},
	"jobs.query": {
		{1, "handle", "uint"},
		{2, "prefix", "str"},
		{3, "state", "uint"},
		{4, "after", "str"},
		{5, "limit", "uint"},
	},
	"jobs.page":    {{1, "more", "bool"}, {2, "after", "str"}},
	"jobs.workers": {{1, "handle", "uint"}, {2, "workers", "uint"}, {3, "timeout", "uint"}, {4, "until idle", "bool"}},

	"blobs.bucket": {{1, "name", "str"}, {2, "default ttl", "uint"}, {3, "max size", "uint"}},
	"blobs.call": {
		{1, "handle", "uint"},
		{2, "owners", "[]key"},
		{3, "key", "str"},
		{4, "to", "str"},
		{5, "content type", "str"},
		{6, "meta", "names"},
		{7, "ttl", "uint"},
		{8, "expire at", "int"},
		{9, "size", "uint"},
		{10, "if match", "str"},
		{11, "if none match", "bool"},
		{12, "prefix", "str"},
		{13, "after", "str"},
		{14, "limit", "uint"},
		{15, "offset", "uint"},
		{16, "length", "uint"},
	},
	"blobs.object": {
		{1, "found", "bool"},
		{2, "key", "str"},
		{3, "size", "uint"},
		{4, "etag", "str"},
		{5, "content type", "str"},
		{6, "modified", "int"},
		{7, "expires", "int"},
		{8, "meta", "names"},
	},
	"blobs.total": {{1, "objects", "int"}, {2, "bytes", "int"}},
	"blobs.page":  {{1, "more", "bool"}, {2, "after", "str"}},

	"sql.database":  {{1, "name", "str"}, {2, "migrations", "[]sql.migration"}},
	"sql.migration": {{1, "name", "str"}, {2, "text", "str"}},
	"sql.statement": {
		{1, "handle", "uint"},
		{2, "sql", "str"},
		{3, "args", "[]sql value"},
		{4, "named", "sql names"},
		{5, "write", "bool"},
		{6, "rows", "bool"},
	},
	"sql.statements": {{1, "handle", "uint"}, {2, "statements", "[]sql.statement"}, {3, "read", "bool"}},
	"sql.done":       {{1, "changes", "int"}, {2, "last id", "int"}},
	"sql.columns":    {{1, "columns", "[]str"}},
	"sql.row":        {{1, "values", "[]sql value"}},
	"sql.result": {
		{1, "changes", "int"}, {2, "last id", "int"}, {3, "columns", "[]str"}, {4, "rows", "[][]sql value"},
	},
	"sql.results": {{1, "results", "[]sql.result"}},

	"records.record": {
		{1, "at", "int"},
		{2, "stream", "text"},
		{3, "name", "text"},
		{4, "level", "int"},
		{5, "body", "text"},
		{6, "trace id", "bin"},
		{7, "span id", "bin"},
		{8, "context", "[]text"},
		{9, "attrs", "[]text"},
	},
	"records.batch": {{1, "records", "[]records.record"}},
	"records.query": {
		{1, "from", "int"},
		{2, "to", "int"},
		{3, "streams", "[]text"},
		{4, "names", "[]text"},
		{5, "min level", "int"},
		{6, "trace id", "bin"},
		{7, "attrs", "[]text"},
		{8, "context", "[]text"},
		{9, "newest", "bool"},
		{10, "limit", "uint"},
		{11, "budget blocks", "uint"},
		{12, "budget bytes", "uint"},
		{13, "budget records", "uint"},
		{14, "search", "str"},
	},
	"records.page":   {{1, "more", "bool"}, {2, "from", "int"}, {3, "to", "int"}},
	"records.cursor": {{1, "segment", "int"}, {2, "row", "int"}, {3, "limit", "uint"}, {4, "expired", "uint"}},
	"records.stream": {{1, "stream", "text"}},
	"records.damage": {
		{1, "stream", "text"},
		{2, "segment", "int"},
		{3, "head row", "int"},
		{4, "from", "int"},
		{5, "to", "int"},
		{6, "reason", "str"},
	},
	"records.damages": {{1, "damages", "[]records.damage"}},

	"metrics.series": {{1, "labels", "names"}, {2, "kind", "str"}, {3, "times", "ints"}, {4, "values", "floats"}},
	"metrics.batch":  {{1, "series", "[]metrics.series"}},
	"metrics.range": {
		{1, "matchers", "names"},
		{2, "from", "int"},
		{3, "to", "int"},
		{4, "limit series", "uint"},
		{5, "limit blocks", "uint"},
		{6, "limit bytes", "uint"},
		{7, "limit decoded", "uint"},
		{8, "limit answered", "uint"},
		{9, "width", "uint"},
		{10, "op", "str"},
		{11, "where", "[]metrics.condition"},
		{12, "by", "[]str"},
		{13, "without", "[]str"},
	},
	"metrics.condition": {{1, "label", "str"}, {2, "kind", "str"}, {3, "values", "[]str"}},
	"metrics.buckets": {
		{1, "labels", "names"},
		{2, "kind", "str"},
		{3, "from", "ints"},
		{4, "to", "ints"},
		{5, "count", "ints"},
		{6, "resets", "ints"},
		{7, "values", "floats"},
		{8, "flags", "bin"},
	},
	"metrics.labels":  {{1, "labels", "names"}},
	"metrics.dropped": {{1, "found", "bool"}, {2, "unreadable groups", "uint"}},
}

var kvCall = []field{
	{1, "handle", "uint"},
	{2, "owners", "[]key"},
	{3, "key", "key"},
	{4, "value", "kv value"},
	{5, "ttl", "uint"},
	{6, "expire at", "int"},
	{7, "if version", "bin"},
	{8, "if absent", "bool"},
	{9, "n", "int"},
	{10, "after", "key"},
	{11, "limit", "uint"},
}

var jobsJob = []field{
	{1, "value", "str"}, {2, "key", "str"}, {3, "at", "int"}, {4, "after", "uint"}, {5, "repeat", "jobs.repeat"},
}

// the methods by the names docs/wire.md gives them
var methods = []struct {
	name   string
	method wire.Method
}{
	{"kv.open", wire.KVOpen},
	{"kv.get", wire.KVGet},
	{"kv.has", wire.KVHas},
	{"kv.set", wire.KVSet},
	{"kv.delete", wire.KVDelete},
	{"kv.take", wire.KVTake},
	{"kv.touch", wire.KVTouch},
	{"kv.add", wire.KVAdd},
	{"kv.max", wire.KVMax},
	{"kv.clear", wire.KVClear},
	{"kv.batch", wire.KVBatch},
	{"kv.view", wire.KVView},
	{"kv.scan", wire.KVScan},
	{"jobs.open", wire.JobsOpen},
	{"jobs.enqueue", wire.JobsEnqueue},
	{"jobs.update", wire.JobsUpdate},
	{"jobs.cancel", wire.JobsCancel},
	{"jobs.get", wire.JobsGet},
	{"jobs.claim", wire.JobsClaim},
	{"jobs.settle", wire.JobsSettle},
	{"jobs.scan", wire.JobsScan},
	{"jobs.work", wire.JobsWork},
	{"blobs.open", wire.BlobsOpen},
	{"blobs.stat", wire.BlobsStat},
	{"blobs.delete", wire.BlobsDelete},
	{"blobs.copy", wire.BlobsCopy},
	{"blobs.move", wire.BlobsMove},
	{"blobs.usage", wire.BlobsUsage},
	{"blobs.clear", wire.BlobsClear},
	{"blobs.scan", wire.BlobsScan},
	{"blobs.put", wire.BlobsPut},
	{"blobs.get", wire.BlobsGet},
	{"sql.open", wire.SQLOpen},
	{"sql.exec", wire.SQLExec},
	{"sql.query", wire.SQLQuery},
	{"sql.batch", wire.SQLBatch},
	{"records.append", wire.RecordsAppend},
	{"records.read", wire.RecordsRead},
	{"records.follow", wire.RecordsFollow},
	{"records.lines", wire.RecordsLines},
	{"records.damaged", wire.RecordsDamaged},
	{"records.drop", wire.RecordsDrop},
	{"metrics.ingest", wire.MetricsIngest},
	{"metrics.read", wire.MetricsRead},
	{"metrics.aggregate", wire.MetricsAggregate},
	{"metrics.drop", wire.MetricsDrop},
}

var codes = []wire.Code{
	wire.CodeInvalid, wire.CodeLimit, wire.CodeClosed, wire.CodeInUse, wire.CodeConflict, wire.CodeCorrupt,
	wire.CodeTooOld, wire.CodeTooNew, wire.CodeSuspended, wire.CodeOutcomeUnknown, wire.CodePermission,
	wire.CodeUnimplemented, wire.CodeCancelled, wire.CodeUnavailable, wire.CodeInternal, wire.CodeProtocol,
	wire.CodeUnauthenticated,
}

// example is a message a vector is written from, and how its bytes read back
// into its Go type
type example struct {
	name, message string
	bytes         []byte
	again         func([]byte) ([]byte, error) // decodes the bytes and writes them again
}

type appender interface {
	Append(dst []byte) []byte
}

// of is the example of a message whose Go type decodes into a pointer to it
func of[M appender, P interface {
	*M
	Decode(body []byte) error
}](name, message string, m M) example {
	return example{name: name, message: message, bytes: m.Append(nil), again: func(b []byte) ([]byte, error) {
		var back M
		if err := P(&back).Decode(b); err != nil {
			return nil, err
		}
		return back.Append(nil), nil
	}}
}

func ptr[T any](v T) *T { return &v }

// the times the examples hold: unix milliseconds, and a record's nanoseconds
const (
	at   = 1_790_000_000_000
	atNs = 1_790_000_000_123_456_789
)

var (
	sixteen = []byte{0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15}
	proof   = mustHex("d8b99f2709a3ca74172cbe93824c1f29b23a0c1e9c21bd851ff2d2c39dbef14e")
	nan1    = math.Float64frombits(0x7ff8000000000001)
)

func mustHex(text string) []byte {
	b, err := hex.DecodeString(text)
	if err != nil {
		panic(err)
	}
	return b
}

func raw(s string) wire.KVValue { return wire.KVValue{Kind: wire.KVBytes, Bytes: []byte(s)} }

func examples() []example {
	return slices.Concat(handshakeExamples(), kvExamples(), jobsExamples(), blobsExamples(), sqlExamples(),
		recordsExamples(), metricsExamples())
}

func handshakeExamples() []example {
	conflict := &wire.Error{Code: wire.CodeConflict, Message: "state changed", What: map[string]string{
		"bucket": "sessions", "key": "7/\xff",
	}}
	return []example{
		of("a HELLO naming every bound, with a token and a challenge", "hello", wire.Hello{
			Protocol: 1, Client: "tinystore-py/0.1", Token: "c2VjcmV0", MaxBody: 64 << 10, StreamCredit: 128 << 10,
			Challenge: sixteen,
		}),
		of("a WELCOME to a local HELLO with a challenge", "welcome", wire.Welcome{
			Protocol: 1, Server: "v0.1.0", Instance: sixteen, Capability: wire.Admin, MaxBody: 1<<20 + 64<<10,
			InFlight: 256, ConnectionCredit: 8 << 20, StreamCredit: 2 << 20,
			Engines: []string{"kv", "jobs", "blobs", "sql", "records", "metrics"}, Now: at, Proof: proof,
		}),
		of("a GOAWAY as the server closes", "goaway", wire.GoAway{
			Code:    wire.CodeUnavailable,
			Message: "the server is closing",
		}),
		{
			name: "a conflict naming a key of bytes that are not UTF-8", message: "error", bytes: conflict.Append(nil),
			again: func(b []byte) ([]byte, error) {
				var back wire.Error
				if err := back.Decode(b); err != nil {
					return nil, err
				}
				return back.Append(nil), nil
			},
		},
		of("a handle", "handle", wire.Handle{Handle: 3}),
		of("an answer that says only that its call was done", "empty", wire.Empty{}),
	}
}

func kvExamples() []example {
	return []example{
		of("a bucket of values that slide", "kv.bucket", wire.KVBucket{Name: "sessions", Sliding: 30 * 86_400_000}),
		of("counters that live in memory for a second", "kv.bucket", wire.KVBucket{
			Name: "login-attempts", Counters: true, DefaultTTL: 900_000, LoseAtMost: 1000,
		}),
		of("kv.get of a key under an owner", "kv.call", wire.KVCall{Handle: 1, Owners: []string{"7"}, Key: "token"}),
		of("kv.set of bytes under an owner of bytes, for an hour, at a version", "kv.call", wire.KVCall{
			Handle: 1, Owners: []string{"tenant", "\xff"}, Key: "token", Value: raw("hello"), TTL: 3_600_000,
			IfVersion: []byte("1"),
		}),
		of("kv.set of an integer if absent, until a time", "kv.call", wire.KVCall{
			Handle: 2, Key: "code", Value: wire.KVValue{Kind: wire.KVInt, Int: -1}, ExpireAt: at, IfAbsent: true,
		}),
		of("kv.add of 1", "kv.call", wire.KVCall{Handle: 3, Owners: []string{"ip"}, Key: "10.0.0.1", N: 1}),
		of("kv.scan of a branch after a key", "kv.call", wire.KVCall{
			Handle: 1, Owners: []string{"7"}, After: "a", Limit: 100,
		}),
		of("an entry holding an integer that expires", "kv.entry", wire.KVEntry{
			Found: true, Value: wire.KVValue{Kind: wire.KVInt, Int: 42}, Version: []byte("7"), Expires: at,
		}),
		of("a scan's entry of empty bytes under a key that is not UTF-8", "kv.entry", wire.KVEntry{
			Found: true, Value: raw(""), Version: []byte("8"), Key: "\xffk",
		}),
		of("an entry of a key that holds nothing", "kv.entry", wire.KVEntry{}),
		of("a page that ends before the branch does", "kv.page", wire.KVPage{More: true, After: "k"}),
		of("kv.batch of a set and a delete", "kv.calls", wire.KVCalls{Calls: []wire.KVOperation{
			{Method: wire.KVSet, KVCall: wire.KVCall{Handle: 1, Key: "a", Value: raw("x")}},
			{Method: wire.KVDelete, KVCall: wire.KVCall{Handle: 1, Key: "b"}},
		}}),
		of("a batch's results", "kv.results", wire.KVResults{Entries: []wire.KVEntry{
			{Found: true, Version: []byte("9")}, {},
		}}),
	}
}

func jobsExamples() []example {
	return []example{
		of("a queue naming its whole policy", "jobs.queue", wire.JobsQueue{
			Name: "sends", Lease: 30_000, MaxAttempts: 20, BackoffFirst: 1000, BackoffMost: 3_600_000,
			MaxWaiting: 1_000_000, KeepFailed: 7 * 86_400_000, KeepDone: 3_600_000,
		}),
		of("a schedule by cron in a zone", "jobs.queue", wire.JobsQueue{
			Name: "purge", Schedule: &wire.Repeat{Cron: "10 3 * * *", Zone: "Europe/Moscow"},
		}),
		of("jobs.enqueue of a job at a time and one repeating every minute", "jobs.batch", wire.JobsBatch{
			Handle: 1, Jobs: []wire.JobsJob{
				{Value: `{"user":42}`, Key: "call:42", At: at},
				{Value: `"ping"`, Key: "ping", After: 60_000, Repeat: &wire.Repeat{Every: 60_000}},
			},
		}),
		of("jobs.update of a value and a time", "jobs.change", wire.JobsChange{
			Handle: 1, JobsJob: wire.JobsJob{Value: `{"user":43}`, Key: "call:42", At: at + 60_000},
		}),
		of("a job by its key", "jobs.key", wire.JobsKey{Handle: 1, Key: "call:42"}),
		of("a failed job", "jobs.entry", wire.JobsEntry{
			Found: true, Key: "call:42", Value: `{"user":42}`, At: at, Attempt: 3, State: 3, Err: "smtp: refused",
			Repeat: "10 3 * * * Europe/Moscow",
		}),
		of("jobs.claim for a minute", "jobs.lease", wire.JobsLease{Handle: 1, Lease: 60_000}),
		of("a held job", "jobs.held", wire.JobsHeld{
			Found: true, Job: 12, Key: "call:42", Value: `{"user":42}`, At: at, Attempt: 1,
		}),
		of("an outcome of every kind", "jobs.outcomes", wire.JobsOutcomes{Outcomes: []wire.JobsOutcome{
			{Job: 12, How: wire.JobAck},
			{Job: 13, How: wire.JobRetry, Err: "busy", After: 5000},
			{Job: 14, How: wire.JobFail, Err: "bad input"},
			{Job: 15, How: wire.JobSnooze, At: at + 100_000},
			{Job: 16, How: wire.JobExtend, After: 30_000},
		}}),
		of("a work stream's outcome", "jobs.outcome", wire.JobsOutcome{
			Job: 1, How: wire.JobRetry, Err: "busy",
			At: at,
		}),
		of("a settlement, one refused", "jobs.settled", wire.JobsSettled{Errors: []*wire.Error{
			nil, {Code: wire.CodeConflict, Message: "the lease ended", What: map[string]string{
				"queue": "sends", "key": "call:42",
			}},
		}}),
		of("jobs.scan of the failed jobs under a prefix", "jobs.query", wire.JobsQuery{
			Handle: 1, Prefix: "chat:42:", State: 3, After: "chat:42:a", Limit: 50,
		}),
		of("a page of jobs", "jobs.page", wire.JobsPage{More: true, After: "chat:42:z"}),
		of("jobs.work of eight workers until idle", "jobs.workers", wire.JobsWorkers{
			Handle: 1, Workers: 8, Timeout: 60_000, UntilIdle: true,
		}),
	}
}

func blobsExamples() []example {
	return []example{
		of("a bucket with a default ttl and a bound", "blobs.bucket", wire.BlobsBucket{
			Name: "avatars", DefaultTTL: 86_400_000, MaxSize: 20 << 20,
		}),
		of("blobs.put creating an object with its type, meta, ttl and size", "blobs.call", wire.BlobsCall{
			Handle: 1, Owners: []string{"users", "7"}, Key: "original.png", ContentType: ptr("image/png"),
			Meta: map[string]string{"camera": "x100"}, TTL: 3_600_000, Size: 70_000, IfNoneMatch: true,
		}),
		of("blobs.move to a key while its ETag matches, until a time", "blobs.call", wire.BlobsCall{
			Handle: 1, Owners: []string{"users", "7"}, Key: "pending/u1", To: "sent/u1", ExpireAt: at,
			IfMatch: `"8f4e"`, Size: -1,
		}),
		of("blobs.scan of a prefix after a key", "blobs.call", wire.BlobsCall{
			Handle: 1, Prefix: "sent/", After: "sent/a", Limit: 100, Size: -1,
		}),
		of("blobs.get of a range", "blobs.call", wire.BlobsCall{
			Handle: 1, Key: "video.mp4", Offset: 1024, Length: 4096, Size: -1,
		}),
		of("an object", "blobs.object", wire.BlobsObject{
			Found: true, Key: "original.png", Size: 70_000, ETag: `"8f4e"`, ContentType: "image/png", Modified: at,
			Expires: at + 3_600_000, Meta: map[string]string{"camera": "x100"},
		}),
		of("a key that holds no object", "blobs.object", wire.BlobsObject{}),
		of("a folder's usage", "blobs.total", wire.BlobsTotal{Objects: 3, Bytes: 210_000}),
		of("a page of objects", "blobs.page", wire.BlobsPage{More: true, After: "sent/z"}),
	}
}

func sqlExamples() []example {
	return []example{
		of("sql.open with two migrations", "sql.database", wire.SQLDatabase{Name: "app", Migrations: []wire.SQLMigration{
			{Name: "001_notes.sql", Text: "create table notes (id integer primary key, title text not null) strict;"},
			{Name: "002_done.sql", Text: "alter table notes add column done integer not null default 0;"},
		}}),
		of("an insert returning its row, with an argument of every kind and a named one", "sql.statement",
			wire.SQLStatement{
				Handle: 1, SQL: "insert into notes (title, author, weight, raw, due, done) values " +
					"(:title, ?, ?, ?, ?, ?) returning *",
				Args:  []any{int64(7), 2.5, []byte{0}, nil, true},
				Named: map[string]any{"title": "milk"}, Write: true,
			}),
		of("sql.batch of an insert and a read of its rows", "sql.statements", wire.SQLStatements{
			Handle: 1, Statements: []wire.SQLStatement{
				{SQL: "insert into notes (title) values (?)", Args: []any{"milk"}},
				{SQL: "select id from notes", Rows: true},
			},
		}),
		of("sql.batch of reads from one snapshot", "sql.statements", wire.SQLStatements{
			Handle: 1, Statements: []wire.SQLStatement{{SQL: "select count(*) from notes"}}, Read: true,
		}),
		of("what a write changed", "sql.done", wire.SQLDone{Changes: 1, LastID: 7}),
		of("a query's columns", "sql.columns", wire.SQLColumns{Columns: []string{"id", "title"}}),
		of("a row of every kind of value", "sql.row", wire.SQLRow{Values: []any{
			int64(7), "milk", nil, 2.5, []byte{1, 2}, "\xff",
		}}),
		of("a batch's results, a write's and a read's", "sql.results", wire.SQLResults{Results: []wire.SQLResult{
			{SQLDone: wire.SQLDone{Changes: 1, LastID: 7}},
			{Columns: []string{"id"}, Rows: [][]any{{int64(7)}}},
		}}),
	}
}

func recordsExamples() []example {
	record := wire.Record{
		At: atNs, Stream: "api", Name: "log", Level: ptr(int64(8)), Body: ptr("disk \xff full"),
		TraceID: sixteen, SpanID: sixteen[:8],
		Context: []wire.RecordField{{Key: "request_id", Value: `"r1"`}},
		Attrs:   []wire.RecordField{{Key: "route", Value: `"/notes"`}, {Key: "ms", Value: "1200"}},
	}
	return []example{
		of("a log line of every field, its body not UTF-8", "records.record", record),
		of("records.append of two", "records.batch", wire.RecordsBatch{Records: []wire.Record{
			record, {At: atNs + 1, Stream: "web", Name: "click", Attrs: []wire.RecordField{
				{Key: "element", Value: `"buy"`},
			}},
		}}),
		of("records.read naming every condition and a budget", "records.query", wire.RecordsQuery{
			From: atNs, To: atNs + 3_600_000_000_000, Streams: []string{"api"}, Names: []string{"log"},
			MinLevel: ptr(int64(4)), TraceID: sixteen, Attrs: []wire.RecordField{{Key: "route", Value: `"/notes"`}},
			Context: []wire.RecordField{{Key: "user", Value: "42"}}, Newest: true, Limit: 100,
			Budget: wire.RecordsBudget{Blocks: 64, Bytes: 8 << 20, Decoded: 100_000}, Search: "connection reset",
		}),
		of("a read's page", "records.page", wire.RecordsPage{More: true, From: atNs, To: atNs + 1_800_000_000_000}),
		of("records.follow from a place", "records.cursor", wire.RecordsCursor{Segment: 12, Row: 340, Limit: 1000}),
		of("a follow's trailer past what retention removed", "records.cursor", wire.RecordsCursor{
			Segment: 13, Expired: 2,
		}),
		of("records.lines of a stream", "records.stream", wire.RecordsStream{Stream: "worker"}),
		of("the damage met, a segment and a head row", "records.damages", wire.RecordsDamages{
			Damages: []wire.RecordsDamage{
				{Stream: "api", Segment: 12, From: atNs, To: atNs + 60_000_000_000, Reason: "a block's CRC-32"},
				{Stream: "api", HeadRow: 99, From: atNs, To: atNs, Reason: "a head row's CRC-32"},
			},
		}),
		of("records.drop of a damaged segment", "records.damage", wire.RecordsDamage{
			Stream: "api", Segment: 12, From: atNs, To: atNs + 60_000_000_000, Reason: "a block's CRC-32",
		}),
	}
}

func metricsExamples() []example {
	cpu := map[string]string{"__name__": "cpu", "host": "web-1"}
	return []example{
		of("metrics.ingest of -0 and a NaN whose payload is 1", "metrics.batch", wire.MetricsBatch{
			Series: []wire.MetricsSeries{{
				Labels: cpu, Kind: "gauge", Times: []int64{at, at + 15_000}, Values: []float64{math.Copysign(0, -1), nan1},
			}},
		}),
		of("a series a read answers", "metrics.series", wire.MetricsSeries{
			Labels: map[string]string{"__name__": "http_requests_total"}, Kind: "counter",
			Times: []int64{at}, Values: []float64{100},
		}),
		of("metrics.read to the open end, within limits of its own", "metrics.range", wire.MetricsRange{
			Matchers: cpu, From: at, To: math.MaxInt64, Limits: wire.MetricsLimits{
				Series: 100, Blocks: 1000, PayloadBytes: 1 << 20, DecodedSamples: 100_000, OutputSamples: 10_000,
			},
		}),
		of("metrics.read of the api hosts' 5xx, but not in dev", "metrics.range", wire.MetricsRange{
			Matchers: map[string]string{"__name__": "http_requests_total"}, From: at, To: math.MaxInt64,
			Where: []wire.MetricsCondition{
				{Label: "env", Kind: "none_of", Values: []string{"dev"}},
				{Label: "host", Kind: "prefix", Values: []string{"api-"}},
				{Label: "status", Kind: "one_of", Values: []string{"500", "502"}},
			},
		}),
		of("metrics.aggregate of an hour's increase", "metrics.range", wire.MetricsRange{
			Matchers: map[string]string{"__name__": "http_requests_total"}, From: at, To: at + 86_400_000,
			Width: 3_600_000, Op: "increase",
		}),
		of("metrics.aggregate of each route's rate, every host joined", "metrics.range", wire.MetricsRange{
			Matchers: map[string]string{"__name__": "http_requests_total"}, From: at, To: at + 86_400_000,
			Width: 3_600_000, Op: "rate", By: []string{"route"},
		}),
		of("metrics.aggregate of a gauge's delta, every series of its name joined", "metrics.range", wire.MetricsRange{
			Matchers: map[string]string{"__name__": "temperature"}, From: at, To: at + 86_400_000,
			Width: 3_600_000, Op: "delta", By: []string{},
		}),
		of("metrics.aggregate of the mean without a host", "metrics.range", wire.MetricsRange{
			Matchers: map[string]string{"__name__": "temperature"}, From: at, To: at + 86_400_000,
			Width: 3_600_000, Op: "avg", Without: []string{"host"},
		}),
		of("an aggregate's buckets, one cut by retention and one overflowed", "metrics.buckets", wire.MetricsBuckets{
			Labels: map[string]string{"__name__": "http_requests_total"}, Kind: "counter",
			Buckets: []wire.MetricsBucket{
				{From: at, To: at + 3_600_000, Count: 240, Resets: 1, Value: 130, Partial: true},
				{From: at + 3_600_000, To: at + 7_200_000, Count: 12, Value: math.Inf(1), Overflow: true},
			},
		}),
		of("metrics.drop of a series", "metrics.labels", wire.MetricsLabels{Labels: cpu}),
		of("what a drop removed", "metrics.dropped", wire.MetricsDropped{Found: true, UnreadableGroups: 1}),
	}
}

// ordered is a JSON object that keeps the order its members were added in
type ordered []member

type member struct {
	name  string
	value any
}

func (o ordered) MarshalJSON() ([]byte, error) {
	var buf bytes.Buffer
	buf.WriteByte('{')
	for i, m := range o {
		if i > 0 {
			buf.WriteByte(',')
		}
		name, err := json.Marshal(m.name)
		if err != nil {
			return nil, err
		}
		value, err := json.Marshal(m.value)
		if err != nil {
			return nil, err
		}
		buf.Write(name)
		buf.WriteByte(':')
		buf.Write(value)
	}
	buf.WriteByte('}')
	return buf.Bytes(), nil
}

// readMessage reads a message's fields through the schema; seen gathers the
// fields every vector holds, message by message
func readMessage(d *wire.Decoder, message string, seen map[string]bool) ordered {
	fields, known := schema[message]
	if !known {
		d.Fail("no schema for %s", message)
		return nil
	}
	got := ordered{}
	for key := range d.Fields() {
		i := slices.IndexFunc(fields, func(f field) bool { return f.key == key })
		if i < 0 {
			d.Fail("%s: key %d, which the schema does not name", message, key)
			return nil
		}
		seen[message+": "+fields[i].name] = true
		got = append(got, member{fields[i].name, readAsKind(d, fields[i].kind, seen)})
	}
	return got
}

func one(kind string, value any) map[string]any { return map[string]any{kind: value} }

func readAsKind(d *wire.Decoder, kind string, seen map[string]bool) any {
	if item, isArray := strings.CutPrefix(kind, "[]"); isArray {
		items := []any{}
		for range d.Items() {
			items = append(items, readAsKind(d, item, seen))
		}
		return one("array", items)
	}
	switch kind {
	case "uint", "int", "float", "bool", "str", "bin":
		return readTyped(d, kind)
	case "text":
		return readTyped(d, typeName(d.Type()))
	case "key":
		return readInteger(d, typeName(d.Type()))
	case "kv value", "sql value":
		if d.Nil() {
			return nil
		}
		return readTyped(d, typeName(d.Type()))
	case "names", "sql names":
		value := "text"
		if kind == "sql names" {
			value = "sql value"
		}
		pairs := []any{}
		for name := range d.Names() {
			pairs = append(pairs, []any{one("str", name), readAsKind(d, value, seen)})
		}
		return one("map", pairs)
	case "ints", "floats":
		return readColumn(d, kind)
	case "error?":
		if d.Nil() {
			return nil
		}
		return one("fields", readMessage(d, "error", seen))
	}
	return one("fields", readMessage(d, kind, seen))
}

func typeName(t wire.Type) string {
	switch t {
	case wire.TypeBool:
		return "bool"
	case wire.TypeInt:
		return "int"
	case wire.TypeFloat:
		return "float"
	case wire.TypeStr:
		return "str"
	case wire.TypeBin:
		return "bin"
	}
	return t.String()
}

// readInteger reads a key, whose integer is written as its value's sign says
func readInteger(d *wire.Decoder, kind string) any {
	if kind != "int" {
		return readTyped(d, kind)
	}
	probe := *d
	if n := probe.Int(); probe.Err() == nil && n < 0 {
		return readTyped(d, "int")
	}
	return readTyped(d, "uint")
}

func readTyped(d *wire.Decoder, kind string) any {
	switch kind {
	case "uint":
		return one("uint", strconv.FormatUint(d.Uint(), 10))
	case "int":
		return one("int", strconv.FormatInt(d.Int(), 10))
	case "float":
		return one("float", fmt.Sprintf("%016x", math.Float64bits(d.Float())))
	case "bool":
		return d.Bool()
	case "str":
		return one("str", d.Str())
	case "bin":
		return one("bin", hex.EncodeToString(d.Bin()))
	}
	d.Fail("a field of kind %s", kind)
	return nil
}

// readColumn reads a column of eight-byte values: integers in decimal, floats
// by their bits in hex, as the values vectors spell them
func readColumn(d *wire.Decoder, kind string) any {
	column := d.Bin()
	if len(column)%8 != 0 {
		d.Fail("a column of %d bytes", len(column))
		return nil
	}
	values := []any{}
	for i := 0; i < len(column); i += 8 {
		bits := binary.LittleEndian.Uint64(column[i:])
		if kind == "ints" {
			values = append(values, strconv.FormatInt(int64(bits), 10))
			continue
		}
		values = append(values, fmt.Sprintf("%016x", bits))
	}
	return one(kind, values)
}

// bases are the messages whose fields a message repeats: a batch's call is a
// kv call with its method, an update a job with its handle
var bases = map[string]string{"kv.operation": "kv.call", "jobs.change": "jobs.job"}

// writeMessages writes messages.json from the examples, a vector a line,
// failing a test on an example whose bytes the schema does not read, whose
// type does not read them back, or on a field no vector holds
func writeMessages(t *testing.T) []byte {
	t.Helper()
	seen := map[string]bool{}
	var vectors [][]byte
	for _, e := range examples() {
		d := wire.NewDecoder(e.bytes)
		fields := readMessage(&d, e.message, seen)
		if err := d.End(); err != nil {
			t.Fatalf("%s: %v", e.name, err)
		}
		if again, err := e.again(e.bytes); err != nil || !bytes.Equal(again, e.bytes) {
			t.Fatalf("%s: read back and written again as %x, %v", e.name, again, err)
		}
		vectors = append(vectors, mustJSON(t, ordered{
			{"name", e.name}, {"message", e.message}, {"hex", hex.EncodeToString(e.bytes)}, {"fields", fields},
		}))
	}
	for message, fields := range schema {
		for _, f := range fields {
			if !seen[message+": "+f.name] && !seen[bases[message]+": "+f.name] {
				t.Errorf("no vector holds %s's %s", message, f.name)
			}
		}
	}
	numbers := ordered{}
	for _, m := range methods {
		numbers = append(numbers, member{m.name, uint16(m.method)})
	}

	var text bytes.Buffer
	text.WriteString("{\n  \"about\": ")
	text.Write(mustJSON(t, "Every message of docs/wire.md, for the server and every SDK: its bytes as the Go "+
		"types write them, and its fields by the names wire.md gives them, in the notation of vectors.json, "+
		"a nested message being {\"fields\": {...}} and a column of eight-byte values {\"ints\": [...]} or "+
		"{\"floats\": [...]}. A field a vector leaves out is absent from its bytes. An SDK decodes each "+
		"vector's hex as its message and finds these fields, and encodes the fields back to the hex. Written "+
		"by TestMessageVectors in server/wire; TINYSTORE_WRITE_VECTORS=1 writes it anew."))
	text.WriteString(",\n  \"methods\": ")
	text.Write(mustJSON(t, numbers))
	text.WriteString(",\n  \"codes\": ")
	text.Write(mustJSON(t, codes))
	text.WriteString(",\n  \"messages\": [\n    ")
	text.Write(bytes.Join(vectors, []byte(",\n    ")))
	text.WriteString("\n  ]\n}\n")
	return text.Bytes()
}

func mustJSON(t *testing.T, v any) []byte {
	t.Helper()
	text, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	return text
}

// messages.json is what the Go types write, read through the schema
func TestMessageVectors(t *testing.T) {
	written := writeMessages(t)
	if os.Getenv("TINYSTORE_WRITE_VECTORS") != "" {
		if err := os.WriteFile(messagesFile, written, 0o600); err != nil {
			t.Fatal(err)
		}
		return
	}
	text, err := os.ReadFile(messagesFile)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(bytes.ReplaceAll(text, []byte("\r\n"), []byte("\n")), written) {
		t.Fatalf("%s is not what the messages write; TINYSTORE_WRITE_VECTORS=1 writes it anew", messagesFile)
	}
}
