package server

import (
	"bytes"
	"context"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/records"
	"github.com/tinyshed/tinystore/server/internal/client"
	"github.com/tinyshed/tinystore/server/wire"
)

func appendRecords(t *testing.T, conn *client.Conn, batch ...wire.Record) error {
	t.Helper()
	_, err := conn.Call(t.Context(), wire.RecordsAppend, wire.RecordsBatch{Records: batch})
	return err
}

// recordsDownload reads a read's or a follow's records, and its trailer into
// last
func recordsDownload(t *testing.T, conn *client.Conn, method wire.Method, ask client.Message,
	last interface{ Decode([]byte) error },
) ([]wire.Record, error) {
	t.Helper()
	st, err := conn.Open(t.Context(), method, ask, true)
	if err != nil {
		return nil, err
	}
	if _, err = st.Response(t.Context()); err != nil {
		return nil, err
	}
	var found []wire.Record
	for {
		body, end, err := st.Next(t.Context())
		if err != nil {
			return found, err
		}
		if end {
			return found, last.Decode(body)
		}
		var record wire.Record
		if err = record.Decode(body); err != nil {
			return found, err
		}
		found = append(found, record)
	}
}

func readRecords(t *testing.T, conn *client.Conn, query wire.RecordsQuery) ([]wire.Record, wire.RecordsPage) {
	t.Helper()
	var page wire.RecordsPage
	found, err := recordsDownload(t, conn, wire.RecordsRead, query, &page)
	if err != nil {
		t.Fatalf("read %+v: %v", query, err)
	}
	return found, page
}

func followRecords(t *testing.T, conn *client.Conn, cursor wire.RecordsCursor) ([]wire.Record, wire.RecordsCursor) {
	t.Helper()
	var next wire.RecordsCursor
	found, err := recordsDownload(t, conn, wire.RecordsFollow, cursor, &next)
	if err != nil {
		t.Fatalf("follow %+v: %v", cursor, err)
	}
	return found, next
}

func levelOf(level int64) *int64 { return &level }

func bodyOf(text string) *string { return &text }

// what went in comes out: times to the nanosecond, levels, bodies byte for
// byte, ids, and fields in their order with their repeated keys
func TestRecordsOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	if !slices.Contains(conn.Welcome.Engines, "records") {
		t.Fatalf("WELCOME serves %v", conn.Welcome.Engines)
	}
	now := time.Now().UnixNano()
	sent := []wire.Record{
		{
			At: now - 3e9 + 7, Stream: "web", Name: "log", Level: levelOf(4), Body: bodyOf("slow request"),
			Context: []wire.RecordField{{Key: "request_id", Value: `"r1"`}},
			Attrs:   []wire.RecordField{{Key: "route", Value: `"/notes"`}, {Key: "ms", Value: "1200.0"}},
		},
		{At: now - 2e9, Stream: "web", Name: "click", TraceID: bytes.Repeat([]byte{7}, 16), SpanID: []byte{
			1, 2, 3, 4, 5,
			6, 7, 8,
		}, Attrs: []wire.RecordField{{Key: "element", Value: `"buy"`}, {Key: "element", Value: `"again"`}}},
		{At: now - 1e9, Stream: "worker", Name: "log", Level: levelOf(8), Body: bodyOf("\xff\xfe not UTF-8")},
	}
	if err := appendRecords(t, conn, sent...); err != nil {
		t.Fatal(err)
	}

	if all, page := readRecords(t, conn, wire.RecordsQuery{}); !reflect.DeepEqual(all, sent) || page.More {
		t.Fatalf("every record: %+v %+v", all, page)
	}
	if warned, _ := readRecords(t, conn, wire.RecordsQuery{Streams: []string{"web"}, MinLevel: levelOf(4)}); len(warned) !=
		1 || warned[0].At != sent[0].At {
		t.Errorf("web's warnings: %+v", warned)
	}
	bought := wire.RecordsQuery{Attrs: []wire.RecordField{{Key: "element", Value: `"buy"`}}, TraceID: sent[1].TraceID}
	if clicks, _ := readRecords(t, conn, bought); len(clicks) != 1 || clicks[0].Name != "click" {
		t.Errorf("by an attribute and a trace: %+v", clicks)
	}

	newest := wire.RecordsQuery{Newest: true, Limit: 1}
	var times []int64
	for range len(sent) {
		found, page := readRecords(t, conn, newest)
		if len(found) != 1 {
			t.Fatalf("a page of %d records", len(found))
		}
		times = append(times, found[0].At)
		newest.From, newest.To = page.From, page.To
	}
	if !slices.Equal(times, []int64{sent[2].At, sent[1].At, sent[0].At}) {
		t.Errorf("paged newest first: %v", times)
	}

	var page wire.RecordsPage
	if _, err := recordsDownload(t, conn, wire.RecordsRead, wire.RecordsQuery{TraceID: []byte{1, 2, 3}}, &page); failureOf(
		err).Code != wire.CodeInvalid {
		t.Errorf("a trace id of three bytes: %v", err)
	}
}

// an append is all or none, and a record refused names its place in the batch
func TestARefusedRecordNamesItsPlace(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	now := time.Now().UnixNano()
	good := wire.Record{At: now, Stream: "web", Name: "click"}

	for _, c := range []struct {
		name   string
		record wire.Record
		code   wire.Code
	}{
		{"a value that is not JSON", wire.Record{
			At: now, Stream: "web", Name: "bad",
			Attrs: []wire.RecordField{{Key: "a", Value: "not json"}},
		}, wire.CodeInvalid},
		{
			"a span id of three bytes",
			wire.Record{At: now, Stream: "web", Name: "bad", SpanID: []byte{1, 2, 3}},
			wire.CodeInvalid,
		},
		{"no stream", wire.Record{At: now, Name: "bad"}, wire.CodeInvalid},
		{
			"an hour ahead of the clock",
			wire.Record{At: now + time.Hour.Nanoseconds(), Stream: "web", Name: "bad"},
			wire.CodeTooNew,
		},
		{"from 1970", wire.Record{At: 1, Stream: "web", Name: "bad"}, wire.CodeTooOld},
	} {
		failure := failureOf(appendRecords(t, conn, good, c.record))
		if failure.Code != c.code || failure.What["call"] != "1" || failure.What["name"] != c.record.Name {
			t.Errorf("%s: %+v", c.name, failure)
		}
	}
	if found, _ := readRecords(t, conn, wire.RecordsQuery{}); len(found) != 0 {
		t.Fatalf("a refused batch wrote %d records", len(found))
	}
}

// testClock is a store's clock a test moves
type testClock struct {
	mu  sync.Mutex
	now time.Time
}

func (c *testClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *testClock) advance(d time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.now = c.now.Add(d)
}

// a follower reads the sealed records from its cursor on, and comes back to
// where it stopped
func TestFollowOverTheWire(t *testing.T) {
	root := t.TempDir()
	clock := &testClock{now: time.Now().UTC()}
	store, err := tinystore.Open(t.Context(), root, tinystore.Options{Manual: true, Clock: clock.Now})
	if err != nil {
		t.Fatal(err)
	}
	ts := serveTestStore(t, root, store, Options{})
	conn := ts.dial(t, wire.Hello{})
	var sent []wire.Record
	for i := range 5 {
		sent = append(sent, wire.Record{At: clock.Now().UnixNano() - int64(5-i), Stream: "web", Name: strconv.Itoa(i)})
	}
	if err = appendRecords(t, conn, sent...); err != nil {
		t.Fatal(err)
	}
	found, waiting := followRecords(t, conn, wire.RecordsCursor{})
	if len(found) != 0 {
		t.Fatalf("a follow before the seal: %d records", len(found))
	}

	clock.advance(2 * time.Hour)
	logs, err := ts.server.recordsStore(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if _, err = logs.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	first, cursor := followRecords(t, conn, wire.RecordsCursor{Segment: waiting.Segment, Row: waiting.Row, Limit: 3})
	rest, end := followRecords(t, conn, wire.RecordsCursor{Segment: cursor.Segment, Row: cursor.Row, Limit: 3})
	if !reflect.DeepEqual(append(first, rest...), sent) {
		t.Fatalf("followed %+v then %+v", first, rest)
	}
	if more, again := followRecords(t, conn, wire.RecordsCursor{Segment: end.Segment, Row: end.Row}); len(more) != 0 ||
		again != end {
		t.Fatalf("a follow at the end: %+v %+v", more, again)
	}
}

// another program's output, cut anywhere, becomes the records Lines makes of
// it: a traceback joined, a JSON line's fields kept
func TestLinesOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	output := "2026-09-26 12:00:01,500 ERROR request failed\nTraceback (most recent call last):\n" +
		"  File \"app.py\", line 3, in handle\nValueError: bad input\n" + `{"level":"warn","msg":"late","ms":12}` + "\n"
	st, err := conn.Open(t.Context(), wire.RecordsLines, wire.RecordsStream{Stream: "worker"}, false)
	if err != nil {
		t.Fatal(err)
	}
	for _, chunk := range []string{output[:17], output[17:60], output[60:]} {
		if err = st.Send(t.Context(), []byte(chunk), chunk == output[60:]); err != nil {
			t.Fatal(err)
		}
	}
	if _, err = st.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	logs, err := ts.server.recordsStore(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if err = logs.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}

	found, _ := readRecords(t, conn, wire.RecordsQuery{Streams: []string{"worker"}})
	if len(found) != 2 || found[0].Name != "log" || *found[0].Body != output[:strings.Index(output, "\n{")] ||
		found[1].Name != "json" || found[1].Level == nil || *found[1].Level != 4 {
		t.Fatalf("the lines as records: %+v", found)
	}
}

// a drop is a repair, an admin's to make; the damaged rows are anyone's to see
func TestADropIsAnAdminsRepair(t *testing.T) {
	data := newToken(t)
	tokens, err := ParseTokens([]byte("data " + data))
	if err != nil {
		t.Fatal(err)
	}
	ts := startTestServer(t, Options{Tokens: tokens})
	admin := ts.dial(t, wire.Hello{})
	remote := ts.dialAt(t, ts.remote, wire.Hello{Token: data})

	body, err := remote.Call(t.Context(), wire.RecordsDamaged, wire.Empty{})
	var damages wire.RecordsDamages
	if err != nil || damages.Decode(body) != nil || len(damages.Damages) != 0 {
		t.Fatalf("the damaged rows of a sound file: %+v, %v", damages, err)
	}
	segment := wire.RecordsDamage{Stream: "web", Segment: 1}
	if _, err = remote.Call(t.Context(), wire.RecordsDrop, segment); failureOf(err).Code != wire.CodePermission {
		t.Errorf("a data connection's drop: %v", err)
	}
	if _, err = admin.Call(t.Context(), wire.RecordsDrop, wire.RecordsDamage{Stream: "web"}); failureOf(err).Code !=
		wire.CodeInvalid {
		t.Errorf("a drop of neither a segment nor a head row: %v", err)
	}
}

// a damaged row a read meets is named as a drop names it
func TestADamagedRowIsNamedAsADropNamesIt(t *testing.T) {
	damaged := &records.DamageError{Damage: records.Damage{
		Stream: "web", Segment: 12, From: time.Unix(0, 5), To: time.Unix(0, 9), Reason: "a block's CRC",
	}, Err: tinystore.ErrCorrupt}
	failure := failure(context.Background(), damaged)
	want := map[string]string{"stream": "web", "segment": "12", "from": "5", "to": "9", "reason": "a block's CRC"}
	if failure.Code != wire.CodeCorrupt || !reflect.DeepEqual(failure.What, want) {
		t.Fatalf("a damaged segment: %+v", failure)
	}
}

// an upload of lines that ends hands over the line it cut, and one the client
// cancels still ends once
func TestLinesHandOverWhatTheyHoldWhenTheUploadEnds(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	st, err := conn.Open(t.Context(), wire.RecordsLines, wire.RecordsStream{Stream: "worker"}, false)
	if err != nil {
		t.Fatal(err)
	}
	if err = st.Send(t.Context(), []byte("started\nhalf a li"), true); err != nil {
		t.Fatal(err)
	}
	if _, err = st.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	logs, err := ts.server.recordsStore(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if err = logs.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	found, _ := readRecords(t, conn, wire.RecordsQuery{Streams: []string{"worker"}})
	if len(found) != 2 || *found[0].Body != "started" || *found[1].Body != "half a li" {
		t.Fatalf("the lines of an upload that ended: %+v", found)
	}

	cancelled, err := conn.Open(t.Context(), wire.RecordsLines, wire.RecordsStream{Stream: "worker"}, false)
	if err != nil {
		t.Fatal(err)
	}
	if err = cancelled.Send(t.Context(), []byte("more\n"), false); err != nil {
		t.Fatal(err)
	}
	if err = cancelled.Cancel(); err != nil {
		t.Fatal(err)
	}
	if _, err = cancelled.Response(t.Context()); failureOf(err).Code != wire.CodeCancelled {
		t.Fatalf("a cancelled upload: %v", err)
	}
}
