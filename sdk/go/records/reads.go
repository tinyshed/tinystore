package records

import (
	"bytes"
	"context"
	"encoding/json"
	"iter"
	"log/slog"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// Query is what a read finds: every condition given holds, and a condition
// left at its zero value is not given. A program that may lack a trace id
// checks it before it asks, since "" reads every trace.
//
//	records.Query{Since: time.Hour, MinLevel: slog.LevelWarn}
//	records.Query{From: start, To: end, Streams: []string{"api"}, Where: records.Where{"user": 42}}
type Query struct {
	// Since is the span before now; or From and To.
	Since time.Duration
	// From is the range's start, included, and To its end, excluded; open when zero.
	From, To time.Time
	// Streams is any of these streams, for every stream's read; a stream reads its own.
	Streams []string
	// Names is any of these names: {"log"} is the lines.
	Names []string
	// MinLevel is this level and worse; an event without a level is not found.
	MinLevel slog.Leveler
	TraceID  string
	Where    Where
	// Search is this text in the message, the name or a value, as a phrase, its case ignored.
	Search string
	// Newest reads newest first; oldest first without it.
	Newest bool
	// Limit is a page's events, 1000 and 10,000 at most; a top's values, ten
	// and 1000 at most; field names' keys.
	Limit int
	// After is a page's Next, with the rest of the query as it was.
	After string

	// Every is the span of an aggregate's row: the range is one without it.
	Every time.Duration
	// Calendar is rows of a calendar in TimeZone, in place of Every:
	// tinystore.Day begins each at a local midnight.
	Calendar tinystore.Calendar
	// TimeZone is the IANA zone a calendar's rows are in: "Europe/Moscow", "UTC".
	TimeZone string
	// By is keys whose every value has rows of its own.
	By []string
	// Stats is fields whose numbers a row sums.
	Stats []string
	// Distinct is fields whose different values a row counts.
	Distinct []string
	// Prefix narrows the keys FieldNames gives.
	Prefix string
}

// asked is the query as the wire carries it, of a stream's handle or every stream's.
func (q Query) asked(handle *uint64) (wire.RecordsQuery, error) {
	out := wire.RecordsQuery{
		Handle: handle, Streams: q.Streams, Names: q.Names, Newest: q.Newest,
		Search: given(q.Search), After: given(q.After),
	}
	if q.Since != 0 {
		ms, err := core.Millis(q.Since, "a query's since")
		if err != nil {
			return out, err
		}
		out.Since = &ms
	}
	if !q.From.IsZero() {
		from := q.From.UnixNano()
		out.From = &from
	}
	if !q.To.IsZero() {
		to := q.To.UnixNano()
		out.To = &to
	}
	if q.MinLevel != nil {
		level := int64(q.MinLevel.Level())
		out.MinLevel = &level
	}
	if q.TraceID != "" {
		trace, err := readTraceID(q.TraceID, "a query's trace id")
		if err != nil {
			return out, err
		}
		id := trace[:]
		out.TraceId = &id
	}
	if q.Limit < 0 {
		return out, core.Errorf(core.Invalid, "a query's limit of %d", q.Limit)
	}
	if q.Limit > 0 {
		limit := uint64(q.Limit)
		out.Limit = &limit
	}
	where, err := q.where()
	out.Where = where
	return out, err
}

// where is each key's value as the JSON it was written as; nil finds nothing
// it was not asked, so it is refused.
func (q Query) where() (map[string]string, error) {
	if q.Where == nil {
		return nil, nil
	}
	out := make(map[string]string, len(q.Where))
	for key, value := range q.Where {
		if value == nil {
			return nil, core.Errorf(core.Invalid, "a query's where %q given as nil: leave it out to read without it", key)
		}
		var spelled bytes.Buffer
		encoder := json.NewEncoder(&spelled)
		encoder.SetEscapeHTML(false)
		if err := encoder.Encode(value); err != nil {
			return nil, core.Wrap(core.Invalid, err, "a query's where %q: JSON cannot write it", key)
		}
		out[key] = string(bytes.TrimSpace(spelled.Bytes()))
	}
	return out, nil
}

func given(text string) *string {
	if text == "" {
		return nil
	}
	return &text
}

// reads is what every read takes: a stream's handle, or every stream without one.
type reads struct {
	store *core.Store
	// opener is the stream's; nil reads every stream.
	opener *core.Opener
}

// ask calls a read's method, its body made for the stream's handle on the connection.
func (r reads) ask(ctx context.Context, method uint16, body func(handle *uint64) []byte) ([]byte, error) {
	if r.opener == nil {
		return r.store.Link.Call(ctx, core.Reading, method, body(nil))
	}
	return core.ViaLink{Link: r.store.Link}.Call(ctx, r.opener, method, func(handle uint64) []byte {
		return body(&handle)
	}, core.Reading)
}

func (r reads) list(ctx context.Context, q Query) (Page, error) {
	var page wire.RecordsPage
	if err := r.query(ctx, wire.MethodRecordsList, q, &page); err != nil {
		return Page{}, err
	}
	out := Page{Events: eventsOf(page.Events)}
	if page.Next != nil {
		out.Next = *page.Next
	}
	return out, nil
}

// query asks a method of a query alone and decodes its answer into m.
func (r reads) query(ctx context.Context, method uint16, q Query, m wire.Message) error {
	var failed error
	answer, err := r.ask(ctx, method, func(handle *uint64) []byte {
		asked, err := q.asked(handle)
		failed = err
		return wire.Encode(&asked)
	})
	if failed != nil {
		return failed
	}
	if err != nil {
		return err
	}
	return decode(answer, m)
}

func (r reads) all(ctx context.Context, q Query) iter.Seq2[Event, error] {
	return func(yield func(Event, error) bool) {
		for {
			page, err := r.list(ctx, q)
			if err != nil {
				yield(Event{}, err)
				return
			}
			for _, event := range page.Events {
				if !yield(event, nil) {
					return
				}
			}
			if page.Next == "" {
				return
			}
			q.After = page.Next
		}
	}
}

func (r reads) count(ctx context.Context, q Query) (int, error) {
	var count wire.RecordsCount
	err := r.query(ctx, wire.MethodRecordsCount, q, &count)
	return int(count.Count), err
}

func (r reads) top(ctx context.Context, key string, q Query) ([]Top, error) {
	if _, err := q.asked(nil); err != nil {
		return nil, err
	}
	answer, err := r.ask(ctx, wire.MethodRecordsTop, func(handle *uint64) []byte {
		asked, _ := q.asked(handle)
		return wire.Encode(&wire.RecordsTopAsked{Query: asked, Key: key})
	})
	if err != nil {
		return nil, err
	}
	var tops wire.RecordsTops
	if err := decode(answer, &tops); err != nil {
		return nil, err
	}
	out := make([]Top, len(tops.Values))
	for i, each := range tops.Values {
		if out[i].Value, err = valueOf(each.Value); err != nil {
			return nil, err
		}
		out[i].Count = int(each.Count)
	}
	return out, nil
}

func (r reads) aggregate(ctx context.Context, q Query) ([]Row, error) {
	if _, err := q.asked(nil); err != nil {
		return nil, err
	}
	var every *uint64
	if q.Every != 0 {
		ms, err := core.Millis(q.Every, "an aggregate's every")
		if err != nil {
			return nil, err
		}
		every = &ms
	}
	answer, err := r.ask(ctx, wire.MethodRecordsAggregate, func(handle *uint64) []byte {
		asked, _ := q.asked(handle)
		message := wire.RecordsAggregateAsked{Query: asked, Every: every, By: q.By, Stats: q.Stats, Distinct: q.Distinct}
		message.Calendar, message.TimeZone = core.CalendarOf(string(q.Calendar), q.TimeZone)
		return wire.Encode(&message)
	})
	if err != nil {
		return nil, err
	}
	var rows wire.RecordsRows
	if err := decode(answer, &rows); err != nil {
		return nil, err
	}
	out := make([]Row, len(rows.Rows))
	for i, row := range rows.Rows {
		if out[i], err = rowOf(row); err != nil {
			return nil, err
		}
	}
	return out, nil
}

func rowOf(row wire.RecordsRow) (Row, error) {
	out := Row{At: time.Unix(0, row.At).UTC(), Count: int(row.Count), By: map[string]any{}, Stats: map[string]Stats{}, Distinct: map[string]int{}}
	for key, spelled := range row.By {
		value, err := valueOf(spelled)
		if err != nil {
			return out, err
		}
		out.By[key] = value
	}
	for key, kept := range row.Stats {
		out.Stats[key] = Stats{
			Samples: int(kept.Samples), Sum: json.Number(kept.Sum), Min: json.Number(kept.Min), Max: json.Number(kept.Max), Avg: kept.Avg,
		}
	}
	for key, count := range row.Distinct {
		out.Distinct[key] = int(count)
	}
	return out, nil
}

// valueOf is a JSON value the store wrote, its numbers a json.Number.
func valueOf(spelled string) (any, error) {
	decoder := json.NewDecoder(bytes.NewReader([]byte(spelled)))
	decoder.UseNumber()
	var value any
	if err := decoder.Decode(&value); err != nil {
		return nil, core.Wrap(core.Protocol, err, "a value the store wrote that does not read")
	}
	return value, nil
}

func (r reads) fieldNames(ctx context.Context, q Query) ([]string, error) {
	if _, err := q.asked(nil); err != nil {
		return nil, err
	}
	answer, err := r.ask(ctx, wire.MethodRecordsFieldNames, func(handle *uint64) []byte {
		asked, _ := q.asked(handle)
		return wire.Encode(&wire.RecordsFieldNamesAsked{Query: asked, Prefix: given(q.Prefix)})
	})
	if err != nil {
		return nil, err
	}
	var names wire.RecordsNames
	err = decode(answer, &names)
	return names.Names, err
}

// tail follows the events written from now on that the query finds, until
// the caller stops, ctx is done or the store closes.
func (r reads) tail(ctx context.Context, q Query) iter.Seq2[TailBatch, error] {
	return func(yield func(TailBatch, error) bool) {
		if _, err := q.asked(nil); err != nil {
			yield(TailBatch{}, err)
			return
		}
		var stream *core.Stream
		err := r.store.Link.Run(ctx, core.Reading, func(c *core.Connection) error {
			var handle *uint64
			if r.opener != nil {
				opened, err := core.HandleOn(ctx, c, r.opener)
				if err != nil {
					return err
				}
				handle = &opened
			}
			asked, _ := q.asked(handle)
			var err error
			stream, err = opened(ctx, c, wire.MethodRecordsTail, wire.Encode(&asked), true)
			return err
		})
		if err != nil {
			// a store that closed before the tail opened ends it as one closed after
			if !closing(r.store) {
				yield(TailBatch{}, err)
			}
			return
		}
		// the store's close waits for the server's end of the tail, which a
		// private child or a sidecar would otherwise drain before it goes
		forget := r.store.OnClose(core.StopFollowers, func() { stream.CancelAndWait(followerWait) })
		defer forget()
		defer stream.Cancel()
		for {
			event, err := stream.Next(ctx)
			if err != nil {
				if !closing(r.store) {
					yield(TailBatch{}, err)
				}
				return
			}
			stream.Consumed(len(event.Body))
			if event.End {
				return
			}
			var batch wire.RecordsTailBatch
			if err := decode(event.Body, &batch); err != nil {
				yield(TailBatch{}, err)
				return
			}
			if !yield(TailBatch{Events: eventsOf(batch.Events), Missed: int(batch.Missed)}, nil) {
				return
			}
		}
	}
}

// followerWait is how long a store's close waits for a follow's end.
const followerWait = 2 * time.Second

// closing says the store's close has begun, which ends a follow as its last
// batch would.
func closing(store *core.Store) bool {
	select {
	case <-store.Closing():
		return true
	default:
		return false
	}
}
