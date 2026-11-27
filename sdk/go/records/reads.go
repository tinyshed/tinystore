package records

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"iter"
	"log/slog"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
	"tinyshed.org/tinystore/sqldb"
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
	// Fill gives a row for every span of the range the stream keeps, an
	// empty one counting 0; it needs Since or From.
	Fill bool
	// Prefix narrows the keys FieldNames gives.
	Prefix string

	// Gap is the pause that ends a session of Sessions: an event later than
	// this after the one before begins the next. By is the keys whose values
	// a session shares.
	Gap time.Duration
	// Max is the longest a session lasts: an event later than this after its
	// first begins the next, pause or none.
	Max time.Duration
	// First is each value's first sessions of the range alone, its later
	// events passed over; every session when zero.
	First int
	// Events is the first events each session keeps, 1000 at most.
	Events int
}

// asked is the query as the wire carries it, of a stream's handle or every
// stream's, and the statements its conditions name by their place, whose
// databases' handles are the connection's it goes on: see on.
func (q Query) asked(handle *uint64) (wire.RecordsQuery, []sqldb.BoundStatement, error) {
	out := wire.RecordsQuery{
		Handle: handle, Streams: q.Streams, Names: q.Names, Newest: q.Newest,
		Search: given(q.Search), After: given(q.After),
	}
	if q.Since != 0 {
		ms, err := core.Millis(q.Since, "a query's since")
		if err != nil {
			return out, nil, err
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
			return out, nil, err
		}
		id := trace[:]
		out.TraceId = &id
	}
	if q.Limit < 0 {
		return out, nil, core.Errorf(core.Invalid, "a query's limit of %d", q.Limit)
	}
	if q.Limit > 0 {
		limit := uint64(q.Limit)
		out.Limit = &limit
	}
	bound := []sqldb.BoundStatement{}
	var err error
	out.Where, out.Conditions, err = q.where(&bound)
	return out, bound, err
}

// where is the query's Where as the wire carries it: a plain value as its
// JSON in where, which every server reads, and a list, a Condition or a
// statement in conditions, which a server older than them refuses rather
// than matching nothing. A statement goes into bound, which its condition
// names by its place. A nil finds nothing it was not asked, so it is refused.
func (q Query) where(bound *[]sqldb.BoundStatement) (map[string]string, map[string]wire.RecordsCondition, error) {
	where, conditions := map[string]string{}, map[string]wire.RecordsCondition{}
	for key, value := range q.Where {
		if value == nil {
			return nil, nil, core.Errorf(core.Invalid, "a query's where %q given as nil: leave it out to read without it", key)
		}
		switch v := value.(type) {
		case Condition:
			if v.err != nil {
				return nil, nil, core.Wrap(core.Invalid, v.err, "a query's where %q", key)
			}
			condition := v.wire
			if v.statement != nil {
				place := uint64(len(*bound))
				*bound = append(*bound, *v.statement)
				condition.Statement = &place
			}
			conditions[key] = condition
			continue
		case sqldb.BoundStatement:
			place := uint64(len(*bound))
			*bound = append(*bound, v)
			conditions[key] = wire.RecordsCondition{Statement: &place}
			continue
		case sqldb.Piece:
			return nil, nil, core.Errorf(core.Invalid,
				"a query's where %q: a stream belongs to no database, so its statement says whose it is: db.Statement(…)", key)
		}
		spelled, items, err := valuesOf(value)
		switch {
		case err != nil:
			return nil, nil, core.Wrap(core.Invalid, err, "a query's where %q", key)
		case items != nil:
			conditions[key] = wire.RecordsCondition{Values: items}
		default:
			where[key] = spelled
		}
	}
	return where, conditions, nil
}

// valuesOf is a value's JSON, and a list's items each as its JSON; an object
// is refused, as the value itself goes inside a list.
func valuesOf(value any) (string, []string, error) {
	var buffer bytes.Buffer
	encoder := json.NewEncoder(&buffer)
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(value); err != nil {
		return "", nil, fmt.Errorf("JSON cannot write it: %w", err)
	}
	spelled := bytes.TrimSpace(buffer.Bytes())
	switch spelled[0] {
	case '[':
		var items []json.RawMessage
		if err := json.Unmarshal(spelled, &items); err != nil {
			return "", nil, err
		}
		texts := make([]string, len(items))
		for i, item := range items {
			texts[i] = string(item)
		}
		return "", texts, nil
	case '{':
		return "", nil, errors.New("an object, where a value, a list or a Condition goes: an object as the value itself goes in a list")
	}
	return string(spelled), nil, nil
}

// Condition is what a key's value must be beside a value of its own: one of
// a list, none of one, or a prefix.
type Condition struct {
	wire wire.RecordsCondition
	err  error
	// statement is the one whose values Not keeps from, in place of a list
	statement *sqldb.BoundStatement
}

// OneOf is a value equal to one of these, each by the JSON it is written
// as; a slice given is its items, as a bare slice in a Where is.
func OneOf(values ...any) Condition {
	items, err := listed(values)
	return Condition{wire: wire.RecordsCondition{Values: items}, err: err}
}

// Not is a value equal to none of these, or the key absent; a statement of
// a database alone, Not(db.Statement(…)), is none of the values it gives.
func Not(values ...any) Condition {
	if len(values) == 1 {
		if statement, ok := values[0].(sqldb.BoundStatement); ok {
			return Condition{wire: wire.RecordsCondition{Not: true}, statement: &statement}
		}
	}
	items, err := listed(values)
	return Condition{wire: wire.RecordsCondition{Values: items, Not: true}, err: err}
}

// Prefix is a text that starts with this one; a number or a bool never does.
func Prefix(text string) Condition {
	return Condition{wire: wire.RecordsCondition{Prefix: &text}}
}

// listed is each value's JSON, a slice's items spread.
func listed(values []any) ([]string, error) {
	out := []string{}
	for _, value := range values {
		spelled, items, err := valuesOf(value)
		if err != nil {
			return nil, err
		}
		if items == nil {
			items = []string{spelled}
		}
		out = append(out, items...)
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

// ask calls a read's method of q, its body made from the query as the wire
// carries it on the connection the call goes on: see on.
func (r reads) ask(ctx context.Context, method uint16, q Query, body func(asked wire.RecordsQuery) []byte) ([]byte, error) {
	asked, bound, err := q.asked(nil)
	if err != nil {
		return nil, err
	}
	if err := r.refuseOthers(bound); err != nil {
		return nil, err
	}
	var answer []byte
	err = r.store.Link.Run(ctx, core.Reading, func(c *core.Connection) error {
		on, err := r.on(ctx, c, asked, bound)
		if err != nil {
			return err
		}
		answer, err = c.Session.Call(ctx, method, body(on))
		return err
	})
	return answer, err
}

// refuseOthers refuses a statement of another store's database: its
// database's handle would be opened on this store's connection, and read
// this store's file of the same name.
func (r reads) refuseOthers(bound []sqldb.BoundStatement) error {
	for _, statement := range bound {
		db, _, _, err := sqldb.CoreBound(statement)
		if err != nil {
			return err
		}
		if home, ok := db.(core.Home); !ok || home.CoreHome().Store != r.store {
			return core.Errorf(core.Invalid, "%s: a statement of another store's database, in a query of this one", db.Describe())
		}
	}
	return nil
}

// on is a query as it goes on a connection: with the stream's handle there,
// and each statement with its database's.
func (r reads) on(ctx context.Context, c *core.Connection, asked wire.RecordsQuery, bound []sqldb.BoundStatement) (wire.RecordsQuery, error) {
	if r.opener != nil {
		handle, err := core.HandleOn(ctx, c, r.opener)
		if err != nil {
			return asked, err
		}
		asked.Handle = &handle
	}
	for _, statement := range bound {
		db, text, values, err := sqldb.CoreBound(statement)
		if err != nil {
			return asked, err
		}
		handle, err := db.Handle(ctx, c)
		if err != nil {
			return asked, err
		}
		asked.Statements = append(asked.Statements, wire.RecordsStatement{Database: handle, Text: text, Values: values})
	}
	return asked, nil
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
	answer, err := r.ask(ctx, method, q, func(asked wire.RecordsQuery) []byte { return wire.Encode(&asked) })
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

// delete is sent again after a lost connection: run again, it deletes what is left.
func (r reads) delete(ctx context.Context, q Query) (int, error) {
	var count wire.RecordsCount
	err := r.query(ctx, wire.MethodRecordsDelete, q, &count)
	return int(count.Count), err
}

func (r reads) top(ctx context.Context, key string, q Query) ([]Top, error) {
	answer, err := r.ask(ctx, wire.MethodRecordsTop, q, func(asked wire.RecordsQuery) []byte {
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

func (r reads) largest(ctx context.Context, key string, q Query) ([]Event, error) {
	answer, err := r.ask(ctx, wire.MethodRecordsLargest, q, func(asked wire.RecordsQuery) []byte {
		return wire.Encode(&wire.RecordsLargestAsked{Query: asked, Key: key})
	})
	if err != nil {
		return nil, err
	}
	var events wire.RecordsEvents
	if err := decode(answer, &events); err != nil {
		return nil, err
	}
	return eventsOf(events.Events), nil
}

func (r reads) aggregate(ctx context.Context, q Query) ([]Row, error) {
	var every *uint64
	if q.Every != 0 {
		ms, err := core.Millis(q.Every, "an aggregate's every")
		if err != nil {
			return nil, err
		}
		every = &ms
	}
	answer, err := r.ask(ctx, wire.MethodRecordsAggregate, q, func(asked wire.RecordsQuery) []byte {
		message := wire.RecordsAggregateAsked{Query: asked, Every: every, By: q.By, Stats: q.Stats, Distinct: q.Distinct, Fill: q.Fill}
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

func (r reads) sessions(ctx context.Context, q Query) ([]Session, error) {
	if q.FirstSessions < 0 || q.MaxEvents < 0 {
		return nil, core.Errorf(core.Invalid, "sessions of FirstSessions %d and MaxEvents %d: neither below 0", q.FirstSessions, q.MaxEvents)
	}
	asked := wire.RecordsSessionsAsked{By: q.By, Events: uint64(q.Events)}
	var err error
	if asked.Gap, err = core.Millis(q.Gap, "a session's gap"); err != nil {
		return nil, err
	}
	if q.Max != 0 {
		longest, err := core.Millis(q.Max, "a session's max")
		if err != nil {
			return nil, err
		}
		asked.Max = &longest
	}
	if q.First > 0 {
		first := uint64(q.First)
		asked.First = &first
	}
	answer, err := r.ask(ctx, wire.MethodRecordsSessions, q, func(query wire.RecordsQuery) []byte {
		message := asked
		message.Query = query
		return wire.Encode(&message)
	})
	if err != nil {
		return nil, err
	}
	var sessions wire.RecordsSessions
	if err := decode(answer, &sessions); err != nil {
		return nil, err
	}
	out := make([]Session, len(sessions.Sessions))
	for i, session := range sessions.Sessions {
		out[i] = Session{
			By: map[string]any{}, From: time.Unix(0, session.From).UTC(), To: time.Unix(0, session.To).UTC(),
			Count: int(session.Count), Events: eventsOf(session.Events), Partial: session.Partial,
		}
		for key, spelled := range session.By {
			if out[i].By[key], err = valueOf(spelled); err != nil {
				return nil, err
			}
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
	if row.Date != nil {
		out.Date = *row.Date
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
	answer, err := r.ask(ctx, wire.MethodRecordsFieldNames, q, func(asked wire.RecordsQuery) []byte {
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
		if _, bound, err := q.asked(nil); err != nil || len(bound) > 0 {
			if err == nil {
				err = core.Errorf(core.Invalid,
					"a tail's where takes no statement: a statement is read once, as a query starts, and a tail lives on")
			}
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
			asked, _, _ := q.asked(handle)
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
