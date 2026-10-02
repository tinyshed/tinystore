package records

import (
	"context"
	"log/slog"
	"testing"
)

// A line logged with a traced context, and a record appended with one, take
// its trace and span; a record that names its own trace keeps it, and the
// caller's records are not changed.
func TestARecordTakesTheTraceOfItsContext(t *testing.T) {
	s := openRecords(t)
	trace, span := TraceID{1, 2, 3}, SpanID{4, 5}
	ctx := WithTrace(t.Context(), trace, span)

	// a line at the test clock's time, which slog's own would be far from
	if err := s.Handler("api").Handle(ctx, slog.NewRecord(testNow, slog.LevelInfo, "charged", 0)); err != nil {
		t.Fatal(err)
	}
	if err := s.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	own := TraceID{9}
	written := []Record{
		{At: testNow, Stream: "api", Name: "paid"},
		{At: testNow, Stream: "api", Name: "elsewhere", TraceID: own},
	}
	if err := s.Append(ctx, written...); err != nil {
		t.Fatal(err)
	}
	if written[0].TraceID != (TraceID{}) {
		t.Fatal("Append changed the caller's record")
	}

	found := map[string]Record{}
	for _, r := range s.readAll(t, Query{}) {
		found[r.Name] = r
	}
	if found["log"].TraceID != trace || found["log"].SpanID != span {
		t.Errorf("a line logged with the trace: %x %x", found["log"].TraceID, found["log"].SpanID)
	}
	if found["paid"].TraceID != trace || found["paid"].SpanID != span || found["elsewhere"].TraceID != own {
		t.Errorf("appended: %x and %x", found["paid"].TraceID, found["elsewhere"].TraceID)
	}
	if got, _ := TraceOf(context.Background()); got != (TraceID{}) {
		t.Errorf("a context of no trace carries %x", got)
	}
}
