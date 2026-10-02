package records

import (
	"context"
	"encoding/json"
	"fmt"
	"math"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Append writes every record or none, in one transaction, and a Scan sees them
// as soon as it returns. A record the format cannot keep, or whose time is past
// retention or more than ClockSkew ahead of the store's clock, is a
// *RecordError naming it. A record of no trace takes ctx's, from WithTrace.
func (s *Store) Append(ctx context.Context, batch ...Record) error {
	batch = traced(ctx, batch)
	if err := checkBatch(batch, s.window(s.now())); err != nil {
		return err
	}
	return s.appendChecked(ctx, batch)
}

// appendChecked appends records that are already checked: Append's, and what the handler queued.
func (s *Store) appendChecked(ctx context.Context, batch []Record) error {
	release, err := s.admitTo(ctx, s.appends)
	if err != nil {
		return err
	}
	defer release()

	if len(batch) == 0 {
		return nil
	}

	unreserve, err := s.reserve(ctx, func() int64 { return appendReservation(batch) })
	if err != nil {
		return err
	}
	defer unreserve()

	s.appendMu.Lock()
	defer s.appendMu.Unlock()
	rows := s.encodeHeadRows(routeToHeads(batch, &s.waiting, unixNanos(s.now())))
	if err = s.writeHeadRows(ctx, rows); err != nil {
		return fmt.Errorf("records: append: %w", err)
	}
	s.waiting.remember(rows)
	s.appended.Add(uint64(len(batch)))
	return nil
}

func checkBatch(batch []Record, accepted window) error {
	input := 0
	for i := range batch {
		err := checkRecord(&batch[i])
		if err == nil {
			err = accepted.check(batch[i].At)
		}
		if err != nil {
			return &RecordError{Index: i, Stream: batch[i].Stream, Name: batch[i].Name, Err: err}
		}
		input += inputSize(&batch[i])
	}
	if input > maxAppendInput {
		return &tinystore.LimitError{
			Name:   "bytes of records in one Append, a segment's; split it",
			Wanted: int64(input), Bound: maxAppendInput,
		}
	}
	return nil
}

// window is the times one call accepts, read once from the store's clock:
//
//	now 12:00, Retention 14 days, ClockSkew 10 minutes → [12:00 fourteen days ago, 12:10]
type window struct {
	oldest, newest int64
}

func (s *Store) window(now time.Time) window {
	return window{oldest: unixNanos(now.Add(-s.opts.Retention)), newest: unixNanos(now.Add(s.opts.ClockSkew))}
}

// check refuses a time the window does not hold, naming the edge it passed
func (w window) check(at time.Time) error {
	switch t := unixNanos(at); {
	case t < w.oldest:
		return fmt.Errorf("%w: time %s is before the retention cutoff %s", tinystore.ErrTooOld, at, timeOf(w.oldest))
	case t > w.newest:
		return fmt.Errorf("%w: time %s is past %s, the store's clock and its skew", tinystore.ErrTooNew, at,
			timeOf(w.newest))
	}
	return nil
}

func timeOf(nanos int64) time.Time {
	return time.Unix(0, nanos).UTC()
}

// the instants a signed count of nanoseconds since 1970 holds
var (
	earliest = time.Unix(0, math.MinInt64)
	latest   = time.Unix(0, math.MaxInt64)
)

// checkRecord refuses what the format could not keep exactly, or within its bounds
func checkRecord(r *Record) error {
	switch {
	case r.Stream == "" || r.Name == "":
		return fmt.Errorf("%w: a record needs a stream and a name", tinystore.ErrInvalid)
	case r.At.Before(earliest) || r.At.After(latest):
		return fmt.Errorf("%w: time %s is past what nanoseconds since 1970 hold", tinystore.ErrInvalid, r.At)
	case r.Level != nil && (*r.Level < minLevel || *r.Level > maxLevel):
		return fmt.Errorf("%w: level %d is past 32 bits", tinystore.ErrInvalid, *r.Level)
	case len(r.Context) > maxFields || len(r.Attrs) > maxFields:
		return fmt.Errorf("%w: more than %d context fields or attributes", tinystore.ErrLimit, maxFields)
	case inputSize(r) > maxBlockInput:
		return &tinystore.LimitError{
			Name: "bytes of a record, a block's", Wanted: int64(inputSize(r)),
			Bound: maxBlockInput,
		}
	}
	for _, fields := range [][]Field{r.Context, r.Attrs} {
		for _, field := range fields {
			if !json.Valid([]byte(field.Value)) {
				return fmt.Errorf("%w: %q is not JSON: %.64q", tinystore.ErrInvalid, field.Key, field.Value)
			}
		}
	}
	return nil
}

// appendReservation covers the records, their serialized copy and its frame
func appendReservation(batch []Record) int64 {
	var input int64
	for i := range batch {
		input += int64(inputSize(&batch[i]))
	}
	return 3 * input
}

func (s *Store) encodeHeadRows(batches []headBatch) []headRow {
	rows := make([]headRow, len(batches))
	for i, batch := range batches {
		rows[i] = s.encodeHeadRow(batch)
	}
	return rows
}

const (
	insertHeadRow = `
		insert into heads (stream, late, first_at, last_at, levels, count, input, size, written_at, body)
		values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`
	// since stays the time of the head's oldest waiting row
	addToHeadState = `
		insert into head_state (stream, late, count, input, since) values (?, ?, ?, ?, ?)
		on conflict (stream, late) do update set count = count + excluded.count, input = input + excluded.input`
)

// writeHeadRows stores every row and what each head now holds, in one
// transaction; a stream seen for the first time gets its id inside it
func (s *Store) writeHeadRows(ctx context.Context, rows []headRow) error {
	writtenAt := unixNanos(s.now())
	added := map[string]int64{}
	err := s.file.UpdatePrepared(ctx, func(tx sqlite.Writer) error {
		for _, row := range rows {
			stream, err := s.streams.resolve(ctx, tx, row.stream, added)
			if err != nil {
				return err
			}
			_, err = tx.ExecContext(ctx, insertHeadRow, stream, row.late, row.first, row.last, row.levels,
				row.count, row.input, len(row.body), writtenAt, row.body)
			if err != nil {
				return err
			}
			_, err = tx.ExecContext(ctx, addToHeadState, stream, row.late, row.count, row.input, writtenAt)
			if err != nil {
				return err
			}
		}
		return nil
	})
	if err == nil {
		s.streams.remember(added)
	}
	return err
}

// unixNanos is t in nanoseconds, clamped to what an int64 holds
func unixNanos(t time.Time) int64 {
	switch {
	case t.Before(earliest):
		return math.MinInt64
	case t.After(latest):
		return math.MaxInt64
	}
	return t.UnixNano()
}
