package records

import (
	"bytes"
	"log/slog"
	"testing"
	"time"
	"unsafe"

	"github.com/tinyshed/tinystore/records/console"
	"github.com/tinyshed/tinystore/records/internal/logline"
)

// a Printer writes a record as a handler's console writes its line: one JSON
// object a line, or the pretty line, and nothing when its console is off
func TestAPrinterWritesARecordAsTheConsoleDoes(t *testing.T) {
	level, body := slog.LevelWarn, "slow request"
	record := Record{
		At: time.Date(2026, 10, 2, 11, 2, 11, 123_000_000, time.UTC), Stream: "api", Name: "log",
		Level: &level, Body: &body, Attrs: []Field{{Key: "ms", Value: "1200"}},
	}
	line := logline.Line{
		At: record.At, Stream: "api", Name: "log", Level: &level, Body: &body,
		Attrs: []logline.Field{{Key: "ms", Value: "1200"}},
	}
	for _, c := range []struct {
		format console.Format
		want   []byte
	}{
		{console.JSON, logline.AppendJSON(nil, &line)},
		{console.Pretty, logline.AppendPretty(nil, &line, logline.Look{Zone: time.Local})},
		{console.Off, nil},
	} {
		var printed bytes.Buffer
		NewPrinter(&printed, c.format).Print(record)
		if !bytes.Equal(printed.Bytes(), c.want) {
			t.Fatalf("format %d printed %q, want %q", c.format, printed.Bytes(), c.want)
		}
	}
}

// a line's field and a record's are one layout, so the handler's lines reach
// the store without a copy
func TestALinesFieldIsARecordsField(t *testing.T) {
	var line logline.Field
	var record Field
	if unsafe.Sizeof(line) != unsafe.Sizeof(record) || unsafe.Offsetof(line.Key) != unsafe.Offsetof(record.Key) ||
		unsafe.Offsetof(line.Value) != unsafe.Offsetof(record.Value) {
		t.Fatal("logline.Field and records.Field differ")
	}
	fields := fieldsOf([]logline.Field{{Key: "ms", Value: "1200"}})
	if len(fields) != 1 || fields[0] != (Field{Key: "ms", Value: "1200"}) {
		t.Fatalf("fieldsOf gave %v", fields)
	}
}
