package spike

import (
	"bytes"
	"encoding/binary"
	"hash/crc32"
	"slices"
	"strconv"
	"testing"
)

func TestRecordNumericPredictionsPreserveResetsAndExceptions(t *testing.T) {
	c := recordTestCodec(t)
	c.features = recordDeepAll
	references, values := make([]string, 200), make([]string, 200)
	for i := range values {
		references[i] = strconv.Itoa(i % 7)
		values[i] = strconv.Itoa((i%7)*100000 + i/7)
	}
	values[9], values[81], values[100] = `"changed type"`, "-17", "-9223372036854775808"
	column, ok := numericRecordColumn(values)
	if !ok {
		t.Fatal("numeric candidate missing")
	}
	for _, mode := range []byte{2, 3} {
		cursor := recordCursor{data: c.pack(c.recordKeyedNumbers(references, column, mode))}
		decoded := c.readRecordPrediction(&cursor, references)
		if err := cursor.finish(); err != nil || !slices.Equal(decoded, values) {
			t.Fatalf("state mode %d: %v", mode, err)
		}
	}
	for i := range values {
		references[i] = strconv.Itoa(i)
		values[i] = strconv.Itoa(i*3 + 7)
	}
	values[113] = "99999"
	column, _ = numericRecordColumn(values)
	cursor := recordCursor{data: c.pack(c.recordAffineNumbers(references, column))}
	decoded := c.readRecordPrediction(&cursor, references)
	if err := cursor.finish(); err != nil || !slices.Equal(decoded, values) {
		t.Fatal("affine residual mismatch", err)
	}
}

func TestRecordCompressedSegmentsKeepAllBounds(t *testing.T) {
	c := recordTestCodec(t)
	c.features = recordDeepAll | recordScope16 | recordOuterCompression
	events := recordStatefulFixture(30)
	base, err := c.encodeRecordSegment([][]recordEvent{events[:15], events[15:]})
	if err != nil {
		t.Fatal(err)
	}
	if base[3] == 4 {
		interior, decodeErr := c.reader.DecodeAll(base[4:len(base)-4], nil)
		if decodeErr != nil {
			t.Fatal(decodeErr)
		}
		base = recordSegmentEnvelope(interior[0], interior[1:])
	}
	wrapped, err := c.compressRecordSegment(base)
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := recordTestCodec(t).decodeRecordSegment(wrapped)
	if err != nil {
		t.Fatal(err)
	}
	assertRecordEvents(t, events, decoded)
	recursive, err := c.compressRecordSegment(wrapped)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = c.decodeRecordSegment(recursive); err == nil {
		t.Fatal("recursive compressed segment accepted")
	}
	for _, plain := range [][]byte{nil, {4}, {9}} {
		bad := recordSegmentEnvelope(4, c.outerWriter.EncodeAll(plain, nil))
		if _, err = c.decodeRecordSegment(bad); err == nil {
			t.Fatal("invalid interior accepted")
		}
	}
	for end := range len(wrapped) {
		if _, err = c.decodeRecordSegment(wrapped[:end]); err == nil {
			t.Fatal("truncation accepted")
		}
	}
}

func FuzzDeepRecordColumn(f *testing.F) {
	c := recordTestCodec(f)
	c.features = recordDeepAll
	seeds := [][]string{
		{"1", "2", "3"},
		{`{"a":[1,true],"b":"x"}`, `{"a":[2,false],"b":"y"}`},
		{`"00010203-0405-0607-0809-0a0b0c0d0e0f"`, `"00010203-0405-0607-0809-0a0b0c0d0e00"`},
	}
	for _, values := range seeds {
		f.Add(c.encodeColumn(values), uint16(len(values)))
		if tree := c.jsonRecordColumn(values); tree != nil {
			f.Add(c.pack(tree), uint16(len(values)))
		}
		f.Add(c.pack(c.typedColumnEncoding(values, true)), uint16(len(values)))
	}
	f.Fuzz(func(t *testing.T, data []byte, count uint16) {
		if len(data) > recordByteLimit || count > 4096 {
			return
		}
		inflated, expanded := 0, 0
		cursor := recordCursor{data: data, inflated: &inflated, expanded: &expanded}
		values := c.decodeColumn(&cursor, int(count))
		if cursor.finish() != nil {
			return
		}
		if len(values) != int(count) {
			t.Fatal("decoded column count")
		}
		roundtrip := recordCursor{data: c.encodeColumn(values)}
		again := c.decodeColumn(&roundtrip, len(values))
		if err := roundtrip.finish(); err != nil || !slices.Equal(again, values) {
			t.Fatal("column mismatch", err)
		}
	})
}

func FuzzDeepRecordSegment(f *testing.F) {
	c := recordTestCodec(f)
	c.features = recordDeepAll | recordScope16 | recordOuterCompression
	events := recordStatefulFixture(12)
	batches := [][]recordEvent{events[:6], events[6:]}
	segment, err := c.encodeRecordSegment(batches)
	if err != nil {
		f.Fatal(err)
	}
	f.Add(segment)
	f.Add(c.bucketRecordSegment(batches))
	if segment[3] != 4 {
		wrapped, _ := c.compressRecordSegment(segment)
		f.Add(wrapped)
	}
	f.Fuzz(func(t *testing.T, input []byte) {
		if len(input) > recordSegmentMaxBlocks*(recordByteLimit+128) {
			return
		}
		data := bytes.Clone(input)
		if len(data) >= 8 {
			binary.LittleEndian.PutUint32(data[len(data)-4:], crc32.ChecksumIEEE(data[:len(data)-4]))
		}
		decoded, decodeErr := c.decodeRecordSegment(data)
		if decodeErr == nil && len(decoded) > recordSegmentMaxBlocks*recordEventLimit {
			t.Fatal("segment event limit")
		}
	})
}
