package records

import (
	"context"
	"slices"

	"github.com/tinyshed/tinystore/records/internal/logline"
)

// WithTrace is ctx carrying a trace and the span inside it. A line logged with
// it through the Handler, and a record appended with it that names no trace
// of its own, take both:
//
//	ctx = records.WithTrace(ctx, trace, span)
//	logger.InfoContext(ctx, "charged")   →  a record of that trace and span
//
// It takes no dependency on a tracing library: OpenTelemetry's ids are the
// same bytes, records.TraceID(span.SpanContext().TraceID()).
func WithTrace(ctx context.Context, trace TraceID, span SpanID) context.Context {
	return logline.WithTrace(ctx, trace, span)
}

// TraceOf is the trace and span ctx carries, both zero when it carries none.
func TraceOf(ctx context.Context) (TraceID, SpanID) {
	trace, span := logline.TraceOf(ctx)
	return trace, span
}

// traced is a batch whose records without a trace take ctx's, copied when one
// changes, so that the caller's records stay as it wrote them.
func traced(ctx context.Context, batch []Record) []Record {
	trace, span := TraceOf(ctx)
	if trace == (TraceID{}) || !slices.ContainsFunc(batch, untraced) {
		return batch
	}
	batch = slices.Clone(batch)
	for i := range batch {
		if untraced(batch[i]) {
			batch[i].TraceID, batch[i].SpanID = trace, span
		}
	}
	return batch
}

func untraced(r Record) bool {
	return r.TraceID == (TraceID{})
}
