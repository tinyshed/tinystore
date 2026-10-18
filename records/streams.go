package records

import (
	"context"
	"iter"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/core"
	"github.com/tinyshed/tinystore/internal/wire"
)

func (e *Events) reads() reads { return reads{store: e.store, opener: e.opener} }

// List is one page of the stream's events, from one moment of its file.
func (e *Events) List(ctx context.Context, q Query) (Page, error) { return e.reads().list(ctx, q) }

// All is every event the query finds, a page after another, holding nothing between pages.
func (e *Events) All(ctx context.Context, q Query) iter.Seq2[Event, error] {
	return e.reads().all(ctx, q)
}

// Count is how many events the query finds.
func (e *Events) Count(ctx context.Context, q Query) (int, error) { return e.reads().count(ctx, q) }

// Top is a key's most frequent values and how many events hold each: ten
// unless Limit asks for more, 1000 at most. The key is a field's or a
// context's, or stream, name or level.
func (e *Events) Top(ctx context.Context, key string, q Query) ([]Top, error) {
	return e.reads().top(ctx, key, q)
}

// Aggregate is a row for each span of Every and each value of the keys of
// By, with its count, the Stats of its fields and their Distinct values.
func (e *Events) Aggregate(ctx context.Context, q Query) ([]Row, error) {
	return e.reads().aggregate(ctx, q)
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

// List is one page of every stream's events, from one moment of the file;
// Streams narrows them.
func (a *AllEvents) List(ctx context.Context, q Query) (Page, error) { return a.reads().list(ctx, q) }

// All is every event the query finds, a page after another, holding nothing between pages.
func (a *AllEvents) All(ctx context.Context, q Query) iter.Seq2[Event, error] {
	return a.reads().all(ctx, q)
}

// Count is how many events the query finds.
func (a *AllEvents) Count(ctx context.Context, q Query) (int, error) { return a.reads().count(ctx, q) }

// Top is a key's most frequent values and how many events hold each: ten
// unless Limit asks for more, 1000 at most. The key is a field's or a
// context's, or stream, name or level.
func (a *AllEvents) Top(ctx context.Context, key string, q Query) ([]Top, error) {
	return a.reads().top(ctx, key, q)
}

// Aggregate is a row for each span of Every and each value of the keys of
// By, with its count, the Stats of its fields and their Distinct values.
func (a *AllEvents) Aggregate(ctx context.Context, q Query) ([]Row, error) {
	return a.reads().aggregate(ctx, q)
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
