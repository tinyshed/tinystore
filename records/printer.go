package records

import (
	"io"

	"github.com/tinyshed/tinystore/records/console"
	"github.com/tinyshed/tinystore/records/internal/logline"
)

// Printer writes records as a logger's console writes its lines: pretty for a
// person at a terminal, its levels in colour where the terminal shows them,
// and one JSON object a line otherwise, unless format says which. The
// tinystore command prints a store's records with one.
type Printer struct {
	echo *logline.Echo // nil prints nothing
}

// NewPrinter prints to w, which is a terminal only when it is a file that is
// one. console.Off prints nothing.
func NewPrinter(w io.Writer, format console.Format) *Printer {
	return &Printer{echo: logline.NewEcho(w, logline.Format(format))}
}

// Print writes a record, a whole line a write.
func (p *Printer) Print(r Record) {
	if p.echo == nil {
		return
	}
	p.echo.Write(&logline.Line{
		At: r.At, Stream: r.Stream, Name: r.Name, Level: r.Level, Body: r.Body,
		TraceID: r.TraceID, SpanID: r.SpanID, Context: lineFields(r.Context), Attrs: lineFields(r.Attrs),
	})
}

func lineFields(fields []Field) []logline.Field {
	line := make([]logline.Field, len(fields))
	for i, f := range fields {
		line[i] = logline.Field(f)
	}
	return line
}
