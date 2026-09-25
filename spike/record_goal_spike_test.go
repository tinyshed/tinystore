package spike

import (
	"bytes"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"os"
	"path/filepath"
	"testing"

	"github.com/klauspost/compress/zstd"
)

func TestStructuredRecordsBelowTenBytes(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 for the density acceptance gate")
	}
	events := recordFixture("frontend", 10_000)
	var raw []byte
	first, last := events[0].at, events[0].at
	for _, event := range events {
		raw = appendRecordEvent(raw, event)
		first, last = min(first, event.at), max(last, event.at)
	}
	digest := sha256.Sum256(raw)
	const expectedInput = "15c447619be8671d3b4559ac29528eadf723dad0e1e8709a52ed07ec9c2e3d02"
	if hex.EncodeToString(digest[:]) != expectedInput {
		t.Fatal("the target corpus changed")
	}
	t.Logf("unchanged input sha256=%s records=%d raw=%d", hex.EncodeToString(digest[:]), len(events), len(raw))
	codec := recordTestCodec(t)
	codec.features = recordDeepAll | recordScope16 | recordOuterCompression
	segment, err := codec.encodeRecordSegment(recordDeepBatches(t, events))
	if err != nil {
		t.Fatal(err)
	}
	blocks := []recordMeasuredBlock{{first: first, last: last, count: len(events), adaptive: segment}}
	path := filepath.Join(t.TempDir(), "records.db")
	size, objects, err := measureRecordFile(t.Context(), path, blocks, true)
	if err != nil {
		t.Fatal(err)
	}
	stat, err := os.Stat(path)
	if err != nil || stat.Size() != size {
		t.Fatal("physical size differs from page accounting", err)
	}
	verifyRecordGoalFile(t, path, events, first, last)
	t.Logf("goal payload=%d file=%d records=%d bytes_per_record=%.4f objects=%s", len(segment), size, len(events), float64(size)/float64(len(events)), objects)
	if size >= int64(10*len(events)) {
		t.Fatalf("density target not achieved: %d >= %d", size, 10*len(events))
	}
	for _, level := range []zstd.EncoderLevel{zstd.SpeedDefault, zstd.SpeedBestCompression} {
		measureGoalZstd(t, raw, level, blocks)
	}
}

func verifyRecordGoalFile(t *testing.T, path string, expected []recordEvent, first, last int64) {
	t.Helper()
	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	var body []byte
	var storedFirst, storedLast int64
	var count int
	const query = `select first_at,last_at,count,body from blocks order by id`
	if err = db.QueryRowContext(t.Context(), query).Scan(&storedFirst, &storedLast, &count, &body); err != nil {
		t.Fatal(err)
	}
	if storedFirst != first || storedLast != last || count != len(expected) {
		t.Fatal("stored event bounds differ")
	}
	fresh := recordTestCodec(t)
	decoded, err := fresh.decodeRecordSegment(body)
	if err != nil {
		t.Fatal(err)
	}
	assertRecordEvents(t, expected, decoded)
	var check string
	if err = db.QueryRowContext(t.Context(), `pragma integrity_check`).Scan(&check); err != nil || check != "ok" {
		t.Fatal("SQLite integrity", check, err)
	}
}

func measureGoalZstd(t *testing.T, raw []byte, level zstd.EncoderLevel, bounds []recordMeasuredBlock) {
	t.Helper()
	writer, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1), zstd.WithEncoderLevel(level),
		zstd.WithWindowSize(recordSegmentMaxBlocks*recordByteLimit), zstd.WithLowerEncoderMem(true))
	if err != nil {
		t.Fatal(err)
	}
	defer writer.Close()
	reader, err := zstd.NewReader(nil, zstd.WithDecoderConcurrency(1), zstd.WithDecoderMaxMemory(recordWorkLimit),
		zstd.WithDecoderMaxWindow(recordSegmentMaxBlocks*recordByteLimit))
	if err != nil {
		t.Fatal(err)
	}
	defer reader.Close()
	compressed := writer.EncodeAll(raw, nil)
	decoded, err := reader.DecodeAll(compressed, nil)
	if err != nil || !bytes.Equal(decoded, raw) {
		t.Fatal("zstd reference differs", err)
	}
	block := bounds[0]
	block.adaptive = recordSegmentEnvelope(2, compressed)
	size, objects, err := measureRecordFile(t.Context(), filepath.Join(t.TempDir(), "rows.db"), []recordMeasuredBlock{block}, true)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("reference=%s payload=%d file=%d bytes_per_record=%.4f objects=%s", level, len(block.adaptive), size, float64(size)/float64(block.count), objects)
}
