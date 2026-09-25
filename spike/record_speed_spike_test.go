package spike

import (
	"encoding/binary"
	"fmt"
	"hash/crc32"
	"testing"

	"github.com/klauspost/compress/zstd"
)

func BenchmarkRecordThroughput(b *testing.B) {
	events := recordFixture("frontend", 10_000)
	batches := recordDeepBatches(b, events)
	inputBytes := 0
	for _, event := range events {
		inputBytes += len(appendRecordEvent(nil, event))
	}
	c := recordTestCodec(b)
	c.features = recordDeepAll | recordScope16 | recordOuterCompression
	body, err := c.encodeRecordSegment(batches)
	if err != nil {
		b.Fatal(err)
	}
	benchmarkRecordDirection(b, "adaptive", recordSpeedCase{
		events, inputBytes, body,
		func() ([]byte, error) { return c.encodeRecordSegment(batches) }, c.decodeRecordSegment,
	})
	for _, level := range []zstd.EncoderLevel{zstd.SpeedDefault, zstd.SpeedBestCompression} {
		writer, writerErr := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1), zstd.WithEncoderLevel(level),
			zstd.WithWindowSize(recordSegmentMaxBlocks*recordByteLimit), zstd.WithLowerEncoderMem(true))
		if writerErr != nil {
			b.Fatal(writerErr)
		}
		defer writer.Close()
		reader, readerErr := zstd.NewReader(nil, zstd.WithDecoderConcurrency(1),
			zstd.WithDecoderMaxWindow(recordSegmentMaxBlocks*recordByteLimit), zstd.WithDecoderMaxMemory(recordWorkLimit))
		if readerErr != nil {
			b.Fatal(readerErr)
		}
		defer reader.Close()
		encode := func() ([]byte, error) {
			raw := make([]byte, 0, inputBytes)
			for _, batch := range batches {
				part, encodeErr := rawRecordEvents(batch)
				if encodeErr != nil {
					return nil, encodeErr
				}
				raw = append(raw, part...)
			}
			return recordSegmentEnvelope(2, writer.EncodeAll(raw, nil)), nil
		}
		decode := func(payload []byte) ([]recordEvent, error) {
			return decodeSpeedRows(reader, payload, len(events), batches)
		}
		encoded, encodeErr := encode()
		if encodeErr != nil {
			b.Fatal(encodeErr)
		}
		benchmarkRecordDirection(b, "zstd_"+level.String(), recordSpeedCase{events, inputBytes, encoded, encode, decode})
	}
}

type recordSpeedCase struct {
	events     []recordEvent
	inputBytes int
	body       []byte
	encode     func() ([]byte, error)
	decode     func([]byte) ([]recordEvent, error)
}

func benchmarkRecordDirection(b *testing.B, name string, fixture recordSpeedCase) {
	b.Run(name+"/encode", func(b *testing.B) { benchmarkRecordEncode(b, fixture) })
	b.Run(name+"/decode", func(b *testing.B) { benchmarkRecordDecode(b, fixture) })
}

func benchmarkRecordEncode(b *testing.B, fixture recordSpeedCase) {
	b.ReportAllocs()
	b.SetBytes(int64(fixture.inputBytes))
	var output []byte
	b.ResetTimer()
	for range b.N {
		var err error
		output, err = fixture.encode()
		if err != nil {
			b.Fatal(err)
		}
	}
	b.StopTimer()
	b.ReportMetric(float64(b.N*len(fixture.events))/b.Elapsed().Seconds(), "records/s")
	decoded, err := fixture.decode(output)
	if err != nil {
		b.Fatal(err)
	}
	assertRecordEvents(b, fixture.events, decoded)
}

func benchmarkRecordDecode(b *testing.B, fixture recordSpeedCase) {
	b.ReportAllocs()
	b.SetBytes(int64(fixture.inputBytes))
	var output []recordEvent
	b.ResetTimer()
	for range b.N {
		var err error
		output, err = fixture.decode(fixture.body)
		if err != nil {
			b.Fatal(err)
		}
	}
	b.StopTimer()
	b.ReportMetric(float64(b.N*len(fixture.events))/b.Elapsed().Seconds(), "records/s")
	assertRecordEvents(b, fixture.events, output)
}

func decodeSpeedRows(reader *zstd.Decoder, payload []byte, count int, batches [][]recordEvent) ([]recordEvent, error) {
	end := len(payload) - 4
	if crc32.ChecksumIEEE(payload[:end]) != binary.LittleEndian.Uint32(payload[end:]) {
		return nil, fmt.Errorf("row checksum mismatch")
	}
	raw, err := reader.DecodeAll(payload[4:end], nil)
	if err != nil {
		return nil, err
	}
	cursor := recordCursor{data: raw}
	events := make([]recordEvent, count)
	for i := range events {
		events[i] = cursor.event()
	}
	if err = cursor.finish(); err != nil {
		return nil, err
	}
	start := 0
	for _, batch := range batches {
		if _, err = rawRecordEvents(events[start : start+len(batch)]); err != nil {
			return nil, err
		}
		start += len(batch)
	}
	return events, nil
}
