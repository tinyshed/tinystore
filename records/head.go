package records

import (
	"encoding/binary"
	"hash/crc32"
	"log/slog"
	"time"
)

// headVersion is the first byte of a head row
const headVersion = 1

// a head row is one stream's flush, in arrival order and under zstd; the
// stream is its table row's, and nothing else is needed to read it:
//
//	version | count | zstd frame of the records | crc-32
//	record  = at | name | presence | level | body | trace | span | context | attributes
type headRow struct {
	stream      string
	late        bool
	first, last int64
	levels      int64
	count       int
	input       int // what its records weigh against a segment's bounds
	body        []byte
}

func (s *Store) encodeHeadRow(batch headBatch) headRow {
	row := headRow{stream: batch.stream, late: batch.late, count: len(batch.records)}
	row.first, row.last = batch.records[0].At.UnixNano(), batch.records[0].At.UnixNano()
	var serialized []byte
	for i := range batch.records {
		r := &batch.records[i]
		at := r.At.UnixNano()
		row.first, row.last = min(row.first, at), max(row.last, at)
		if r.Level != nil {
			row.levels |= levelBit(int64(*r.Level))
		}
		row.input += inputSize(r)
		serialized = appendHeadRecord(serialized, r)
	}
	body := appendCount([]byte{headVersion}, row.count)
	body = s.heads.EncodeAll(serialized, body)
	row.body = binary.LittleEndian.AppendUint32(body, crc32.ChecksumIEEE(body))
	return row
}

func appendHeadRecord(out []byte, r *Record) []byte {
	out = binary.AppendVarint(out, r.At.UnixNano())
	out = appendString(out, r.Name)
	presence := presenceOf(r)
	out = append(out, presence)
	if r.Level != nil {
		out = binary.AppendVarint(out, int64(*r.Level))
	}
	if r.Body != nil {
		out = appendString(out, *r.Body)
	}
	if presence&hasTrace != 0 {
		out = append(out, r.TraceID[:]...)
	}
	if presence&hasSpan != 0 {
		out = append(out, r.SpanID[:]...)
	}
	return appendFields(appendFields(out, r.Context), r.Attrs)
}

// parseHeadRow is the one parser of a head row
func (d *decoder) parseHeadRow(stream string, body []byte) ([]Record, error) {
	if len(body) < 5 || body[0] != headVersion {
		return nil, corrupt("head row version")
	}
	end := len(body) - 4
	if crc32.ChecksumIEEE(body[:end]) != binary.LittleEndian.Uint32(body[end:]) {
		return nil, corrupt("head row checksum")
	}
	c := cursor{data: body[1:end]}
	count := c.count(maxBlockRecords)
	if c.err != nil || count == 0 {
		return nil, corrupt("head row count")
	}
	serialized, err := d.zstd.DecodeAll(c.data, nil)
	if err != nil {
		return nil, corrupt("head row frame")
	}
	records := make([]Record, count)
	c = cursor{data: serialized, text: string(serialized)}
	for i := range records {
		records[i] = readHeadRecord(&c, stream)
	}
	return records, c.finish()
}

func readHeadRecord(c *cursor, stream string) Record {
	r := Record{At: time.Unix(0, c.varint()).UTC(), Stream: stream, Name: c.string(maxBlockInput)}
	presence := c.readByte()
	if presence&^(hasLevel|hasBody|hasTrace|hasSpan|hasContext) != 0 {
		c.fail("head record presence")
	}
	if presence&hasLevel != 0 {
		level := c.varint()
		if level < minLevel || level > maxLevel {
			c.fail("level out of range")
		}
		r.Level = new(slog.Level(level))
	}
	if presence&hasBody != 0 {
		r.Body = new(c.string(maxBlockInput))
	}
	if presence&hasTrace != 0 {
		copy(r.TraceID[:], c.take(len(r.TraceID)))
	}
	if presence&hasSpan != 0 {
		copy(r.SpanID[:], c.take(len(r.SpanID)))
	}
	r.Context, r.Attrs = readFields(c), readFields(c)
	if (presence&hasContext != 0) != (len(r.Context) > 0) {
		c.fail("head record context")
	}
	return r
}

func readFields(c *cursor) []Field {
	count := c.count(maxFields)
	if count == 0 {
		return nil
	}
	fields := make([]Field, count)
	for i := range fields {
		fields[i] = Field{Key: c.string(maxBlockInput), Value: c.string(maxBlockInput)}
	}
	return fields
}
