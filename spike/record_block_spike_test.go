package spike

import (
	"bytes"
	"encoding/binary"
	"errors"
	"fmt"
	"hash/crc32"

	"github.com/klauspost/compress/zstd"
)

type recordBlockCodec struct {
	writer      *zstd.Encoder
	reader      *zstd.Decoder
	predict     bool
	features    uint32
	outerWriter *zstd.Encoder
}

func newRecordBlockCodec() (*recordBlockCodec, error) {
	writer, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1),
		zstd.WithEncoderLevel(zstd.SpeedDefault), zstd.WithWindowSize(recordByteLimit), zstd.WithLowerEncoderMem(true))
	if err != nil {
		return nil, err
	}
	reader, err := zstd.NewReader(nil, zstd.WithDecoderConcurrency(1),
		zstd.WithDecoderMaxMemory(recordWorkLimit), zstd.WithDecoderMaxWindow(recordByteLimit))
	if err != nil {
		writer.Close()
		return nil, err
	}
	return &recordBlockCodec{writer: writer, reader: reader, predict: true}, nil
}

func (c *recordBlockCodec) close() {
	c.writer.Close()
	c.reader.Close()
	if c.outerWriter != nil {
		c.outerWriter.Close()
	}
}

func (c *recordBlockCodec) pack(data []byte) []byte {
	compressed := c.writer.EncodeAll(data, nil)
	mode, payload := byte(0), data
	if len(compressed) < len(data) {
		mode, payload = 1, compressed
	}
	out := binary.AppendUvarint([]byte{mode}, uint64(len(data)))
	out = binary.AppendUvarint(out, uint64(len(payload)))
	return append(out, payload...)
}

func (c *recordBlockCodec) unpack(cursor *recordCursor) []byte {
	mode := cursor.number(1)
	size := cursor.number(recordWorkLimit)
	if cursor.inflated != nil {
		*cursor.inflated += size
		if *cursor.inflated > recordWorkLimit {
			cursor.fail("total inflated bytes")
		}
	}
	payload := cursor.take(cursor.number(recordWorkLimit))
	if cursor.err != nil {
		return nil
	}
	if mode == 1 {
		var err error
		payload, err = c.reader.DecodeAll(payload, nil)
		if err != nil {
			cursor.fail("compressed stream")
			return nil
		}
	}
	if len(payload) != size {
		cursor.fail("decoded stream length")
	}
	return payload
}

func rawRecordEvents(events []recordEvent) ([]byte, error) {
	if len(events) == 0 || len(events) > recordEventLimit {
		return nil, errors.New("record block event count")
	}
	var raw []byte
	cells := 0
	for _, event := range events {
		if err := checkRecordEvent(event); err != nil {
			return nil, err
		}
		raw = appendRecordEvent(raw, event)
		cells += len(event.context) + len(event.attrs) + 8
		if len(raw) > recordByteLimit || cells > recordCellLimit {
			return nil, errors.New("record block byte or cell limit")
		}
	}
	return raw, nil
}

func recordEnvelope(mode byte, count, rawBytes int, payload []byte) []byte {
	out := binary.AppendUvarint([]byte{'R', 'E', 'C', 1, mode}, uint64(count))
	out = binary.AppendUvarint(out, uint64(rawBytes))
	out = append(out, payload...)
	return binary.LittleEndian.AppendUint32(out, crc32.ChecksumIEEE(out))
}

func (c *recordBlockCodec) baseline(events []recordEvent) ([]byte, error) {
	raw, err := rawRecordEvents(events)
	if err != nil {
		return nil, err
	}
	return recordEnvelope(0, len(events), len(raw), c.pack(raw)), nil
}

func (c *recordBlockCodec) encode(events []recordEvent) ([]byte, error) {
	raw, err := rawRecordEvents(events)
	if err != nil {
		return nil, err
	}
	mode, best := byte(0), c.pack(raw)
	for _, tuple := range []bool{false, true} {
		for _, shared := range []bool{false, true} {
			layout, ok := planRecordColumns(events, tuple, shared)
			if !ok {
				continue
			}
			candidate := c.encodeRecordLayout(layout)
			if len(candidate) < len(best) {
				mode, best = 1, candidate
			}
		}
	}
	return recordEnvelope(mode, len(events), len(raw), best), nil
}

func (c *recordBlockCodec) decode(block []byte) ([]recordEvent, error) {
	if len(block) < 11 || len(block) > recordByteLimit+64 || !bytes.Equal(block[:4], []byte{'R', 'E', 'C', 1}) {
		return nil, errors.New("record block header or size")
	}
	end := len(block) - 4
	if crc32.ChecksumIEEE(block[:end]) != binary.LittleEndian.Uint32(block[end:]) {
		return nil, errors.New("record block checksum")
	}
	inflated, expanded := 0, 0
	cursor := recordCursor{data: block[4:end], inflated: &inflated, expanded: &expanded}
	mode := cursor.number(1)
	count, rawBytes := cursor.number(recordEventLimit), cursor.number(recordByteLimit)
	if count == 0 || rawBytes == 0 {
		return nil, errors.New("record block empty bounds")
	}
	var events []recordEvent
	if mode == 0 {
		rows := recordCursor{data: c.unpack(&cursor)}
		for range count {
			events = append(events, rows.event())
		}
		if err := rows.finish(); err != nil {
			return nil, err
		}
	} else {
		events = c.decodeRecordLayout(&cursor, count)
	}
	if err := cursor.finish(); err != nil {
		return nil, err
	}
	raw, err := rawRecordEvents(events)
	if err != nil || len(raw) != rawBytes {
		return nil, errors.New("record block reconstructed bounds")
	}
	return events, nil
}

type recordCursor struct {
	data     []byte
	err      error
	inflated *int
	expanded *int
	depth    int
}

func (r *recordCursor) reserveText(size int) bool {
	if r.expanded != nil {
		*r.expanded += size
		if *r.expanded > recordWorkLimit {
			r.fail("expanded column byte limit")
		}
	}
	return r.err == nil
}

func (r *recordCursor) fail(invariant string) {
	if r.err == nil {
		r.err = fmt.Errorf("invalid record block: %s", invariant)
	}
}

func (r *recordCursor) unsigned() uint64 {
	if r.err != nil {
		return 0
	}
	value, n := binary.Uvarint(r.data)
	if n <= 0 {
		r.fail("unsigned integer")
		return 0
	}
	r.data = r.data[n:]
	return value
}

func (r *recordCursor) number(limit int) int {
	value := r.unsigned()
	if value > uint64(limit) {
		r.fail("count or length exceeds limit")
		return 0
	}
	return int(value)
}

func (r *recordCursor) signed() int64 {
	value := r.unsigned()
	return int64(value>>1) ^ -int64(value&1)
}

func (r *recordCursor) take(count int) []byte {
	if r.err != nil || count > len(r.data) {
		r.fail("truncated bytes")
		return nil
	}
	value := r.data[:count]
	r.data = r.data[count:]
	return value
}

func (r *recordCursor) text() string {
	return string(r.take(r.number(recordByteLimit)))
}

func (r *recordCursor) fields() []recordField {
	count := r.number(recordFieldLimit)
	fields := make([]recordField, count)
	for i := range fields {
		fields[i] = recordField{r.text(), r.text()}
	}
	return fields
}

func (r *recordCursor) event() recordEvent {
	event := recordEvent{at: r.signed(), stream: r.text(), name: r.text()}
	presence := r.number(15)
	if presence&1 != 0 {
		level := r.signed()
		event.level = &level
	}
	if presence&2 != 0 {
		body := r.text()
		event.body = &body
	}
	if presence&4 != 0 {
		event.traceID = bytes.Clone(r.take(16))
	}
	if presence&8 != 0 {
		event.spanID = bytes.Clone(r.take(8))
	}
	event.context, event.attrs = r.fields(), r.fields()
	return event
}

func (r *recordCursor) finish() error {
	if len(r.data) != 0 {
		r.fail("trailing bytes")
	}
	return r.err
}
