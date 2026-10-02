package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"hash/maphash"
	"io"
	"log/slog"
	"maps"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/tinyshed/tinystore/records"
	"github.com/tinyshed/tinystore/server/reach"
	"github.com/tinyshed/tinystore/server/wire"
)

const logsUsage = `usage:
  tinystore logs [dir] [-f] [--level warn] [--since 1h] [--grep text] [--stream api] [-n 100] [--json]

The application's logs and events, the last -n of them and, with -f, those
that arrive after them, a record sent up to ten seconds late included, read
through the server serving dir, which starts when none does. A line is pretty
on a terminal and one JSON object otherwise.`

// how often -f asks for the records that arrived
const followEvery = time.Second

// followOverlap is how far behind the newest record printed -f reads again,
// so that a record another process sent a little late is printed too; the
// records engine counts one ten seconds behind its stream's newest as late.
const followOverlap = 10 * time.Second

type logsFlags struct {
	dir, level, grep, stream string
	since                    time.Duration
	limit                    int
	follow, json             bool
}

func parseLogs(args []string, stderr io.Writer) (logsFlags, error) {
	var asked logsFlags
	flags := flag.NewFlagSet("logs", flag.ContinueOnError)
	flags.SetOutput(stderr)
	flags.Usage = func() { fmt.Fprintln(stderr, logsUsage) }
	flags.StringVar(&asked.dir, "dir", "", "")
	flags.BoolVar(&asked.follow, "f", false, "")
	flags.StringVar(&asked.level, "level", "", "")
	flags.DurationVar(&asked.since, "since", 0, "")
	flags.StringVar(&asked.grep, "grep", "", "")
	flags.StringVar(&asked.stream, "stream", "", "")
	flags.IntVar(&asked.limit, "n", 100, "")
	flags.BoolVar(&asked.json, "json", false, "")
	dir, err := parseWithDir(flags, args, &asked.dir)
	if err != nil {
		return logsFlags{}, err
	}
	asked.dir = dir
	if asked.limit < 1 || asked.limit > 10_000 {
		return logsFlags{}, errors.New("logs -n is 1 to 10000 records")
	}
	return asked, nil
}

// logs prints the last records of a store, and with -f those that arrive after
// them, until ctx ends
func logs(ctx context.Context, args []string, out io.Writer, stderr io.Writer) error {
	asked, err := parseLogs(args, stderr)
	if err != nil {
		return err
	}
	started := time.Now()
	query, err := recordsQuery(asked, started)
	if err != nil {
		return err
	}
	conn, err := reachStore(ctx, asked.dir)
	if err != nil {
		return err
	}
	defer conn.Close() // the records are printed; a close that fails changes none of them

	console := records.Console(0)
	if asked.json {
		console = records.ConsoleJSON
	}
	printer := records.NewPrinter(out, console)
	printed := newShown()
	oldest, err := printLast(ctx, conn, query, printer, printed)
	if err != nil || !asked.follow {
		return err
	}
	if oldest == 0 {
		oldest = max(query.From, started.Add(-followOverlap).UnixNano())
	}
	return follow(ctx, conn, query, oldest, printer, printed)
}

// recordsQuery is what the flags ask of a read: the newest records first, so
// that the last of them are the ones read
func recordsQuery(asked logsFlags, now time.Time) (wire.RecordsQuery, error) {
	query := wire.RecordsQuery{Search: asked.grep, Newest: true}
	query.Limit = uint64(asked.limit) //nolint:gosec // 1 to 10000, as parseLogs checked
	if asked.since > 0 {
		query.From = now.Add(-asked.since).UnixNano()
	}
	if asked.stream != "" {
		query.Streams = []string{asked.stream}
	}
	if asked.level != "" {
		level, err := levelOf(asked.level)
		if err != nil {
			return query, err
		}
		query.MinLevel = &level
	}
	return query, nil
}

// levelOf is a level by slog's name or its number
func levelOf(text string) (int64, error) {
	var level slog.Level
	if err := level.UnmarshalText([]byte(strings.ToUpper(text))); err == nil {
		return int64(level), nil
	}
	if n, err := strconv.ParseInt(text, 10, 64); err == nil {
		return n, nil
	}
	return 0, fmt.Errorf("logs --level %q: debug, info, warn or error", text)
}

// shown is what logs printed with a time the next read reaches, a count for
// each record's bytes, so that a record read again is passed over and two
// alike are both printed
type shown struct {
	seed   maphash.Seed
	counts map[uint64]shownRecord
	newest int64
}

type shownRecord struct {
	at    int64
	count int
}

func newShown() *shown {
	return &shown{seed: maphash.MakeSeed(), counts: map[uint64]shownRecord{}}
}

// fresh counts r as one read meets it, in that read's counts, and says
// whether it is new: a read meets a record printed n times n times before it
// meets one more alike, which it then counts as printed
func (s *shown) fresh(r wire.Record, read map[uint64]int) bool {
	key := maphash.Bytes(s.seed, r.Append(nil))
	read[key]++
	printed := s.counts[key]
	if read[key] <= printed.count {
		return false
	}
	s.counts[key] = shownRecord{at: r.At, count: printed.count + 1}
	s.newest = max(s.newest, r.At)
	return true
}

// forget drops the records printed before from, which no read reaches again
func (s *shown) forget(from int64) {
	maps.DeleteFunc(s.counts, func(_ uint64, r shownRecord) bool { return r.at < from })
}

// printLast prints the records the query reads, oldest first, and says the
// time of the oldest, zero when there is none: -f prints nothing before it,
// since the query passed over those
func printLast(ctx context.Context, conn *reach.Conn, query wire.RecordsQuery, printer *records.Printer,
	printed *shown,
) (int64, error) {
	found, _, err := readRecords(ctx, conn, query)
	if err != nil || len(found) == 0 {
		return 0, err
	}
	slices.Reverse(found)
	read := map[uint64]int{}
	for _, r := range found {
		printed.fresh(r, read)
		if err = printRecord(printer, r); err != nil {
			return 0, err
		}
	}
	return found[0].At, nil
}

// follow prints the records that arrive after those printed, asking every
// second, until ctx ends. Each read starts followOverlap behind the newest
// printed and never before oldest, and passes over what was printed.
func follow(ctx context.Context, conn *reach.Conn, query wire.RecordsQuery, oldest int64,
	printer *records.Printer, printed *shown,
) error {
	query.Newest, query.Limit = false, 1000
	for {
		query.From, query.To = max(oldest, printed.newest-int64(followOverlap)), 0
		err := printNew(ctx, conn, query, printer, printed)
		if err != nil && ctx.Err() == nil { // a read Ctrl+C ended is no failure
			return err
		}
		printed.forget(query.From)
		select {
		case <-ctx.Done():
			return nil
		case <-time.After(followEvery):
		}
	}
}

// printNew prints the records the query reads that were not printed before,
// a page at a time
func printNew(ctx context.Context, conn *reach.Conn, query wire.RecordsQuery, printer *records.Printer,
	printed *shown,
) error {
	read := map[uint64]int{}
	for {
		found, page, err := readRecords(ctx, conn, query)
		if err != nil {
			return err
		}
		for _, r := range found {
			if !printed.fresh(r, read) {
				continue
			}
			if err = printRecord(printer, r); err != nil {
				return err
			}
		}
		if !page.More {
			return nil
		}
		query.From, query.To = page.From, page.To
	}
}

// readRecords reads one page: its records, and the page that ended it
func readRecords(ctx context.Context, conn *reach.Conn, query wire.RecordsQuery) ([]wire.Record, wire.RecordsPage,
	error,
) {
	st, err := conn.Open(ctx, wire.RecordsRead, query, true)
	if err != nil {
		return nil, wire.RecordsPage{}, err
	}
	if _, err = st.Response(ctx); err != nil {
		return nil, wire.RecordsPage{}, err
	}
	var found []wire.Record
	for {
		body, last, err := st.Next(ctx)
		if err != nil {
			return nil, wire.RecordsPage{}, err
		}
		if last {
			var page wire.RecordsPage
			return found, page, page.Decode(body)
		}
		var r wire.Record
		if err = r.Decode(body); err != nil {
			return nil, wire.RecordsPage{}, err
		}
		found = append(found, r)
	}
}

func printRecord(printer *records.Printer, sent wire.Record) error {
	r, err := recordOf(sent)
	if err == nil {
		printer.Print(r)
	}
	return err
}

// recordOf is a record as the wire carries it, as records keeps it
func recordOf(sent wire.Record) (records.Record, error) {
	r := records.Record{
		At: time.Unix(0, sent.At), Stream: sent.Stream, Name: sent.Name, Body: sent.Body,
		Context: fieldsOf(sent.Context), Attrs: fieldsOf(sent.Attrs),
	}
	if sent.Level != nil {
		level := slog.Level(*sent.Level)
		r.Level = &level
	}
	if len(sent.TraceID) > 0 && copy(r.TraceID[:], sent.TraceID) != len(sent.TraceID) ||
		len(sent.SpanID) > 0 && copy(r.SpanID[:], sent.SpanID) != len(sent.SpanID) {
		return r, errors.New("a record's trace or span id is not its length")
	}
	return r, nil
}

func fieldsOf(sent []wire.RecordField) []records.Field {
	fields := make([]records.Field, len(sent))
	for i, field := range sent {
		fields[i] = records.Field{Key: field.Key, Value: field.Value}
	}
	return fields
}
