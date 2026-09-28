package wire_test

import (
	"errors"
	"reflect"
	"testing"

	"github.com/tinyshed/tinystore/server/wire"
)

// every records message reads back as it was written
func TestRecordsMessagesReadBackAsTheyWereWritten(t *testing.T) {
	warn, body, empty := int64(4), "slow request", ""
	record := wire.Record{
		At: 1_790_000_000_123_456_789, Stream: "web", Name: "log", Level: &warn, Body: &body,
		TraceID: make([]byte, 16), SpanID: []byte{1, 2, 3, 4, 5, 6, 7, 8},
		Context: []wire.RecordField{{Key: "request_id", Value: `"r1"`}, {Key: "request_id", Value: `"r2"`}},
		Attrs:   []wire.RecordField{{Key: "ms", Value: "1200.0"}},
	}
	messages := []struct {
		written interface{ Append([]byte) []byte }
		read    interface{ Decode([]byte) error }
	}{
		{
			wire.RecordsBatch{Records: []wire.Record{record, {At: -1, Stream: "s", Name: "n", Body: &empty}}},
			&wire.RecordsBatch{},
		},
		{wire.RecordsQuery{
			From: 1, To: 2, Streams: []string{"web"}, Names: []string{"log", "click"}, MinLevel: &warn,
			TraceID: make([]byte, 16), Attrs: record.Attrs, Context: record.Context, Newest: true, Limit: 10,
			Budget: wire.RecordsBudget{Blocks: 1, Bytes: 2, Decoded: 3},
		}, &wire.RecordsQuery{}},
		{wire.RecordsPage{More: true, From: 5, To: 9}, &wire.RecordsPage{}},
		{wire.RecordsCursor{Segment: 7, Row: 1023, Limit: 1000, Expired: 2}, &wire.RecordsCursor{}},
		{wire.RecordsStream{Stream: "worker"}, &wire.RecordsStream{}},
		{wire.RecordsDamages{Damages: []wire.RecordsDamage{
			{Stream: "web", Segment: 3, From: 1, To: 2, Reason: "a block's CRC"},
			{Stream: "web", HeadRow: 9},
		}}, &wire.RecordsDamages{}},
		{wire.RecordsDamage{Stream: "web", HeadRow: 9}, &wire.RecordsDamage{}},
	}
	for _, m := range messages {
		if err := m.read.Decode(m.written.Append(nil)); err != nil {
			t.Errorf("%T: %v", m.written, err)
			continue
		}
		if got := reflect.ValueOf(m.read).Elem().Interface(); !reflect.DeepEqual(got, m.written) {
			t.Errorf("%T read as %+v", m.written, got)
		}
	}
}

// a record's text that is not UTF-8, as another program's output may be,
// travels as bin and comes back byte for byte; fields come in pairs
func TestARecordsTextIsItsBytes(t *testing.T) {
	body := "\xff\xfe line"
	record := wire.Record{
		Stream: "worker", Name: "log", Body: &body,
		Attrs: []wire.RecordField{{Key: "\xff", Value: `"\xfe"`}},
	}
	var read wire.Record
	if err := read.Decode(record.Append(nil)); err != nil || *read.Body != body || read.Attrs[0] != record.Attrs[0] {
		t.Fatalf("read as %+v, %v", read, err)
	}

	m := wire.BeginMap(nil)
	m.Key(9)
	m.SetBuf(wire.AppendStr(wire.AppendArray(m.Buf(), 1), "lonely"))
	if err := (&wire.Record{}).Decode(m.End()); !errors.Is(err, wire.ErrMessage) {
		t.Fatalf("a key without its value: %v", err)
	}
}
