package records

import (
	"context"
	"iter"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

func (e *Events) reads() reads { return reads{store: e.store, opener: e.opener} }

// List is one page of the stream's events: Limit of them that match, fewer
// only on the last page, whose Next is "".
func (e *Events) List(ctx context.Context, q Query) (Page, error) { return e.reads().list(ctx, q) }

// All is every event the query finds, a page after another, holding nothing between pages.
func (e *Events) All(ctx context.Context, q Query) iter.Seq2[Event, error] {
	return e.reads().all(ctx, q)
}

// Count is how many events the query finds.
func (e *Events) Count(ctx context.Context, q Query) (int, error) { return e.reads().count(ctx, q) }

// Delete deletes every event the query finds, of those appended before the
// call, and what the store keeps only for them: a context of their own,
// their keys and counts, the bytes they lay in. It answers how many it
// deleted. It takes a Where, and no Search, Newest, Limit or After. It
// cannot be undone, and run again it deletes what is left: no poll's cursor
// moves, and an event's id stays held until the stream's keep passes its
// time. A backup taken before it, and another node's store, keep the events.
func (e *Events) Delete(ctx context.Context, q Query) (int, error) { return e.reads().delete(ctx, q) }

// Top is a key's most frequent values and how many events hold each: ten
// unless Limit asks for more, 1000 at most. The key is a field's or a
// context's, or stream, name, level or message.
func (e *Events) Top(ctx context.Context, key string, q Query) ([]Top, error) {
	return e.reads().top(ctx, key, q)
}

// Largest is the events whose value at key is a number, the largest first:
// ten unless Limit asks for more, 1000 at most. The key is a field's, else
// a context's; an event without a number there is passed over, and events
// of one number come in the order List gives them.
func (e *Events) Largest(ctx context.Context, key string, q Query) ([]Event, error) {
	return e.reads().largest(ctx, key, q)
}

// Aggregate is a row for each span of Every and each value of the keys of
// By, with its count, the Stats of its fields and their Distinct values.
func (e *Events) Aggregate(ctx context.Context, q Query) ([]Row, error) {
	return e.reads().aggregate(ctx, q)
}

// Sessions is the events the query finds cut into sessions by a pause: the
// events of one value of each key of By, each within Gap of the one before,
// in the order of their first events. The answer is whole, not paged, and
// ErrLimit past 10,000 sessions held, open ones among them, or 64 MiB of
// what they keep.
//
//	visits, err := events.Sessions(ctx, records.Query{Since: 30 * 24 * time.Hour,
//		By: []string{"userId"}, Gap: 30 * time.Minute, Max: 4 * time.Hour, First: 1, Events: 50})
func (e *Events) Sessions(ctx context.Context, q Query) ([]Session, error) {
	return e.reads().sessions(ctx, q)
}

// FieldNames is the keys the events hold, in fields and in context, sorted;
// Prefix narrows them.
func (e *Events) FieldNames(ctx context.Context, q Query) ([]string, error) {
	return e.reads().fieldNames(ctx, q)
}

// Tail is the events written from now on that the query finds, a batch as
// they are written. A reader slower than the writers skips what left the
// store's hold, and the batch after says how many. It ends when the loop is
// left, ctx is done or the store closes.
func (e *Events) Tail(ctx context.Context, q Query) iter.Seq2[TailBatch, error] {
	return e.reads().tail(ctx, q)
}

// AllEvents is every stream's events, to read, and what a store holds of them.
type AllEvents struct {
	store *core.Store
}

// OpenAllEvents is every stream's events of the store, to read.
func OpenAllEvents(store *tinystore.Store) (*AllEvents, error) {
	return &AllEvents{store: store.CoreHome().Store}, nil
}

func (a *AllEvents) reads() reads { return reads{store: a.store} }

// List is one page of every stream's events, as a stream's List reads it;
// Streams narrows them.
func (a *AllEvents) List(ctx context.Context, q Query) (Page, error) { return a.reads().list(ctx, q) }

// All is every event the query finds, a page after another, holding nothing between pages.
func (a *AllEvents) All(ctx context.Context, q Query) iter.Seq2[Event, error] {
	return a.reads().all(ctx, q)
}

// Count is how many events the query finds.
func (a *AllEvents) Count(ctx context.Context, q Query) (int, error) { return a.reads().count(ctx, q) }

// Delete deletes every event the query finds in the streams it names, or in
// every stream, as a stream's Delete does:
//
//	deleted, err := events.Delete(ctx, records.Query{Streams: []string{"activity"}, Where: records.Where{"userId": 97516}})
func (a *AllEvents) Delete(ctx context.Context, q Query) (int, error) {
	return a.reads().delete(ctx, q)
}

// Top is a key's most frequent values and how many events hold each: ten
// unless Limit asks for more, 1000 at most. The key is a field's or a
// context's, or stream, name, level or message.
func (a *AllEvents) Top(ctx context.Context, key string, q Query) ([]Top, error) {
	return a.reads().top(ctx, key, q)
}

// Largest is the events whose value at key is a number, the largest first,
// as a stream's Largest gives them.
func (a *AllEvents) Largest(ctx context.Context, key string, q Query) ([]Event, error) {
	return a.reads().largest(ctx, key, q)
}

// Aggregate is a row for each span of Every and each value of the keys of
// By, with its count, the Stats of its fields and their Distinct values.
func (a *AllEvents) Aggregate(ctx context.Context, q Query) ([]Row, error) {
	return a.reads().aggregate(ctx, q)
}

// Sessions is the events the query finds cut into sessions by a pause, as a
// stream's Sessions cuts them.
func (a *AllEvents) Sessions(ctx context.Context, q Query) ([]Session, error) {
	return a.reads().sessions(ctx, q)
}

// FieldNames is the keys the events hold, in fields and in context, sorted;
// Prefix narrows them.
func (a *AllEvents) FieldNames(ctx context.Context, q Query) ([]string, error) {
	return a.reads().fieldNames(ctx, q)
}

// Tail is the events written from now on that the query finds, a batch as
// they are written. A reader slower than the writers skips what left the
// store's hold, and the batch after says how many. It ends when the loop is
// left, ctx is done or the store closes.
func (a *AllEvents) Tail(ctx context.Context, q Query) iter.Seq2[TailBatch, error] {
	return a.reads().tail(ctx, q)
}

// Streams is every stream with events, by name, with what it keeps, where
// that begins and its newest event's time.
func (a *AllEvents) Streams(ctx context.Context) ([]Stream, error) {
	answer, err := a.store.Link.Call(ctx, core.Reading, wire.MethodRecordsStreams, wire.Encode(&wire.Empty{}))
	if err != nil {
		return nil, err
	}
	var streams wire.RecordsStreams
	if err := decode(answer, &streams); err != nil {
		return nil, err
	}
	out := make([]Stream, len(streams.Streams))
	for i, each := range streams.Streams {
		out[i] = Stream{
			Name: each.Name, Keep: time.Duration(each.Keep) * time.Millisecond,
			RetainedFrom: time.Unix(0, each.RetainedFrom).UTC(), Newest: time.Unix(0, each.Newest).UTC(),
		}
	}
	return out, nil
}

// Fields is what the events of stream carry, none of it read from a packed
// event: each name with how many events have it, and each key of their
// fields and contexts with the kinds of value it holds and how many events
// carry it. A stream with no events carries nothing.
func (a *AllEvents) Fields(ctx context.Context, stream string) (StreamFields, error) {
	asked := wire.RecordsFieldsAsked{Stream: stream}
	answer, err := a.store.Link.Call(ctx, core.Reading, wire.MethodRecordsFields, wire.Encode(&asked))
	if err != nil {
		return StreamFields{}, err
	}
	var carried wire.RecordsStreamFields
	if err := decode(answer, &carried); err != nil {
		return StreamFields{}, err
	}
	out := StreamFields{
		EventNames: make([]NameCount, len(carried.EventNames)),
		Fields:     fieldInfos(carried.Fields),
		Context:    fieldInfos(carried.Context),
	}
	for i, each := range carried.EventNames {
		out.EventNames[i] = NameCount{Name: each.Name, Count: int(each.Count)}
	}
	return out, nil
}

func fieldInfos(fields []wire.RecordsFieldInfo) []FieldInfo {
	out := make([]FieldInfo, len(fields))
	for i, each := range fields {
		kinds := make([]Kind, len(each.Kinds))
		for k, kind := range each.Kinds {
			kinds[k] = Kind(kind)
		}
		out[i] = FieldInfo{Name: each.Name, Kinds: kinds, Count: int(each.Count)}
	}
	return out
}

// Poll is every event once, a batch a call, from a cursor the program keeps,
// the oldest event kept from "". With no event past the cursor, the batch is
// empty and its cursor the one given. It reads what the store has packed,
// which a stream's waiting events are once a segment's worth or an hour old.
func (a *AllEvents) Poll(ctx context.Context, cursor string) (Batch, error) {
	asked := wire.RecordsPollAsked{Cursor: given(cursor)}
	answer, err := a.store.Link.Call(ctx, core.Reading, wire.MethodRecordsPoll, wire.Encode(&asked))
	if err != nil {
		return Batch{}, err
	}
	var batch wire.RecordsBatch
	if err := decode(answer, &batch); err != nil {
		return Batch{}, err
	}
	return Batch{Events: eventsOf(batch.Events), Cursor: batch.Cursor, Expired: int(batch.Expired)}, nil
}

// Damaged is what reads met that no longer reads, which Drop removes: each
// head row and segment with its times.
func (a *AllEvents) Damaged(ctx context.Context) ([]Damage, error) {
	answer, err := a.store.Link.Call(ctx, core.Reading, wire.MethodRecordsDamaged, wire.Encode(&wire.Empty{}))
	if err != nil {
		return nil, err
	}
	var damages wire.RecordsDamages
	if err := decode(answer, &damages); err != nil {
		return nil, err
	}
	out := make([]Damage, len(damages.Damages))
	for i, each := range damages.Damages {
		out[i] = Damage{
			Part: each.Part, ID: int64(each.Id), Stream: each.Stream,
			First: time.Unix(0, each.First).UTC(), Last: time.Unix(0, each.Last).UTC(), Error: each.Error,
		}
	}
	return out, nil
}

// Drop removes a head row or a segment that Damaged listed, so that the
// reads over it work again; one that still reads is never dropped.
func (a *AllEvents) Drop(ctx context.Context, damage Damage) error {
	body := wire.RecordsDamage{
		Part: damage.Part, Id: uint64(max(damage.ID, 0)), Stream: damage.Stream,
		First: damage.First.UnixNano(), Last: damage.Last.UnixNano(), Error: damage.Error,
	}
	_, err := a.store.Link.Call(ctx, core.Writing, wire.MethodRecordsDrop, wire.Encode(&body))
	return err
}
