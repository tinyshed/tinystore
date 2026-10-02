package server

import (
	"errors"
	"fmt"
	"log/slog"
	"math"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/records"
	"github.com/tinyshed/tinystore/server/wire"
)

func (s *Server) recordsMethods(methods map[wire.Method]handler) {
	methods[wire.RecordsAppend] = recordsAppend
	methods[wire.RecordsRead] = recordsRead
	methods[wire.RecordsFollow] = recordsFollow
	methods[wire.RecordsLines] = recordsLines
	methods[wire.RecordsDamaged] = recordsDamaged
	methods[wire.RecordsDrop] = recordsDrop
}

// recordsAppend writes its records in one transaction, all or none
func recordsAppend(c *call) error {
	var ask wire.RecordsBatch
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	logs, err := c.session.server.recordsStore(c.ctx)
	if err != nil {
		return err
	}
	batch := make([]records.Record, len(ask.Records))
	for i, sent := range ask.Records {
		if batch[i], err = recordOf(sent); err != nil {
			return &records.RecordError{Index: i, Stream: sent.Stream, Name: sent.Name, Err: err}
		}
	}
	if err = logs.Append(c.ctx, batch...); err != nil {
		return err
	}
	return respond(c, wire.Empty{})
}

// recordsRead is a download: one page, a record a DATA, and a last DATA
// saying the range the next page reads
func recordsRead(c *call) error {
	var ask wire.RecordsQuery
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	logs, err := c.session.server.recordsStore(c.ctx)
	if err != nil {
		return err
	}
	query, err := queryOf(ask)
	if err != nil {
		return err
	}
	page, err := logs.Scan(c.ctx, query)
	if err != nil {
		return err
	}
	release, err := c.holdAnswer(func() int64 { return weighRecords(page.Records) })
	if err != nil {
		return err
	}
	defer release()

	if err = sendRecords(c, page.Records); err != nil {
		return err
	}
	return trailer(c, wire.RecordsPage{More: page.More, From: nanosOf(page.Next.From), To: nanosOf(page.Next.To)})
}

// recordsFollow is a download: the sealed records after a cursor, a record a
// DATA, and a last DATA holding the cursor the next follow begins at
func recordsFollow(c *call) error {
	var ask wire.RecordsCursor
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	logs, err := c.session.server.recordsStore(c.ctx)
	if err != nil {
		return err
	}
	after := records.Cursor{Segment: ask.Segment, Row: int(min(ask.Row, math.MaxInt32))}
	batch, err := logs.Follow(c.ctx, after, int(min(ask.Limit, math.MaxInt32)))
	if err != nil {
		return err
	}
	release, err := c.holdAnswer(func() int64 { return weighRecords(batch.Records) })
	if err != nil {
		return err
	}
	defer release()

	if err = sendRecords(c, batch.Records); err != nil {
		return err
	}
	return trailer(c, wire.RecordsCursor{
		Segment: batch.Next.Segment, Row: int64(batch.Next.Row), Expired: uint64(max(batch.Expired, 0)),
	})
}

func sendRecords(c *call, found []records.Record) error {
	if err := begin(c, wire.Empty{}); err != nil {
		return err
	}
	for _, record := range found {
		if err := item(c, wireRecord(record)); err != nil {
			return err
		}
	}
	return nil
}

// recordsLines is an upload: another program's output, cut anywhere, becomes
// records of a stream as Lines makes them.
//
// The writer is closed however the upload ends, which hands over the record it
// holds, since the lines it was given were written as a program's output is. A
// cancel or a lost connection ends only what had not reached it.
func recordsLines(c *call) error {
	var ask wire.RecordsStream
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	if ask.Stream == "" {
		return fmt.Errorf("%w: records: lines of no stream", tinystore.ErrInvalid)
	}
	logs, err := c.session.server.recordsStore(c.ctx)
	if err != nil {
		return err
	}
	lines := logs.Lines(ask.Stream)
	if err = writeLines(c, lines); err != nil {
		return errors.Join(err, lines.Close())
	}
	if err = lines.Close(); err != nil {
		return err
	}
	return respond(c, wire.Empty{})
}

func writeLines(c *call, lines interface{ Write([]byte) (int, error) }) error {
	for last := false; !last; {
		body, end, err := c.receive()
		if err != nil {
			return err
		}
		_, err = lines.Write(body)
		c.consumed(body)
		if err != nil {
			return err
		}
		last = end
	}
	return nil
}

// recordsDamaged lists the rows the server's records have met that no longer
// read
func recordsDamaged(c *call) error {
	if err := (&wire.Empty{}).Decode(c.request); err != nil {
		return err
	}
	logs, err := c.session.server.recordsStore(c.ctx)
	if err != nil {
		return err
	}
	found := logs.Damaged()
	damages := wire.RecordsDamages{Damages: make([]wire.RecordsDamage, len(found))}
	for i, damage := range found {
		damages.Damages[i] = wireDamage(damage)
	}
	return respond(c, damages)
}

// recordsDrop removes a damaged row, a repair, which is an admin's to make
func recordsDrop(c *call) error {
	var ask wire.RecordsDamage
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	if c.session.capability != wire.Admin {
		return fmt.Errorf("%w: records: a drop is a repair", errAdminOnly)
	}
	logs, err := c.session.server.recordsStore(c.ctx)
	if err != nil {
		return err
	}
	damage := records.Damage{
		Stream: ask.Stream, Segment: ask.Segment, HeadRow: ask.HeadRow, From: timeOf(ask.From), To: timeOf(ask.To),
		Reason: ask.Reason,
	}
	if err = logs.Drop(c.ctx, damage); err != nil {
		return err
	}
	return respond(c, wire.Empty{})
}

func recordOf(sent wire.Record) (records.Record, error) {
	record := records.Record{
		At: time.Unix(0, sent.At).UTC(), Stream: sent.Stream, Name: sent.Name, Body: sent.Body,
		Context: fieldsOf(sent.Context), Attrs: fieldsOf(sent.Attrs),
	}
	if sent.Level != nil {
		level := slog.Level(*sent.Level)
		record.Level = &level
	}
	if err := idInto(record.TraceID[:], sent.TraceID, "trace"); err != nil {
		return record, err
	}
	err := idInto(record.SpanID[:], sent.SpanID, "span")
	return record, err
}

// idInto copies a trace or a span id of exactly its bytes into id; none
// leaves it zero
func idInto(id, sent []byte, what string) error {
	if len(sent) == 0 {
		return nil
	}
	if len(sent) != len(id) {
		return fmt.Errorf("%w: a %s id of %d bytes, not %d", tinystore.ErrInvalid, what, len(sent), len(id))
	}
	copy(id, sent)
	return nil
}

func fieldsOf(sent []wire.RecordField) []records.Field {
	if len(sent) == 0 {
		return nil
	}
	fields := make([]records.Field, len(sent))
	for i, field := range sent {
		fields[i] = records.Field{Key: field.Key, Value: field.Value}
	}
	return fields
}

func wireRecord(record records.Record) wire.Record {
	sent := wire.Record{
		At: record.At.UnixNano(), Stream: record.Stream, Name: record.Name, Body: record.Body,
		Context: wireFields(record.Context), Attrs: wireFields(record.Attrs),
	}
	if record.Level != nil {
		level := int64(*record.Level)
		sent.Level = &level
	}
	if record.TraceID != (records.TraceID{}) {
		sent.TraceID = record.TraceID[:]
	}
	if record.SpanID != (records.SpanID{}) {
		sent.SpanID = record.SpanID[:]
	}
	return sent
}

func wireFields(fields []records.Field) []wire.RecordField {
	sent := make([]wire.RecordField, len(fields))
	for i, field := range fields {
		sent[i] = wire.RecordField{Key: field.Key, Value: field.Value}
	}
	return sent
}

func wireDamage(damage records.Damage) wire.RecordsDamage {
	return wire.RecordsDamage{
		Stream: damage.Stream, Segment: damage.Segment, HeadRow: damage.HeadRow, From: nanosOf(damage.From),
		To: nanosOf(damage.To), Reason: damage.Reason,
	}
}

// queryOf is a query as the engine takes it: no streams or names is every one
func queryOf(sent wire.RecordsQuery) (records.Query, error) {
	query := records.Query{
		From: timeOf(sent.From), To: timeOf(sent.To), Attrs: fieldsOf(sent.Attrs), Context: fieldsOf(sent.Context),
		Newest: sent.Newest, Limit: int(min(sent.Limit, math.MaxInt32)), Search: sent.Search,
		Budget: records.Budget{
			Blocks: int(min(sent.Budget.Blocks, math.MaxInt32)), Bytes: int(min(sent.Budget.Bytes, math.MaxInt32)),
			Decoded: int(min(sent.Budget.Decoded, math.MaxInt32)),
		},
	}
	if len(sent.Streams) > 0 {
		query.Streams = sent.Streams
	}
	if len(sent.Names) > 0 {
		query.Names = sent.Names
	}
	if sent.MinLevel != nil {
		level := slog.Level(*sent.MinLevel)
		query.MinLevel = &level
	}
	err := idInto(query.TraceID[:], sent.TraceID, "trace")
	return query, err
}

// timeOf is unix nanoseconds as a time, zero for an open end
func timeOf(nanos int64) time.Time {
	if nanos == 0 {
		return time.Time{}
	}
	return time.Unix(0, nanos).UTC()
}

func nanosOf(t time.Time) int64 {
	if t.IsZero() {
		return 0
	}
	return t.UnixNano()
}
