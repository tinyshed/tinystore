// Package records keeps an application's logs and events in records.db inside
// a tinystore.Store. A record lives in one of two places, and every read sees
// both from one snapshot:
//
//	Append, Handler ──► head ──── Maintain ────► segments
//	                    zstd rows, seals a head   a row per segment, a row per
//	                    durable    when full or   block of up to 1024 records in
//	                               an hour old    event-time order
//
//	Read   ◄── candidate blocks and head rows, decoded outside the snapshot
//	Follow ◄── sealed segments after a cursor, in the order they were sealed
//
// Start with records.go, types.go and options.go; every other public method
// opens the file of its step:
//
//	records.go     Open, Close, Stats, Snapshot: the handle and its lifecycle
//	types.go       Record, Query, Page, Cursor, Batch and the errors
//	field.go       Field and the helpers that spell values as JSON
//	options.go     options, the format's bounds and their defaults
//	admission.go   the open gate, and reservations in the store's memory
//	streams.go     stream names and the ids the tables use
//
//	append.go      Append: check → admit → reserve → route → write the heads
//	route.go       a batch split by stream, lateness and a row's bounds
//	head.go        the head row: one stream's flush under zstd; one parser
//	handler.go     the slog.Handler: map, check, queue; Flush
//	lines.go       Lines: another program's lines, joined into records and queued
//	level.go       where the programs writing lines put their level
//
//	maintain.go    Maintain: expire, seal what is ready, merge what it sealed small
//	seal.go        a head read a segment's worth at a time, and encoded
//	publish.go     segment, blocks, filters and keys in, head rows out: one transaction
//	merge.go       a stream's small segments, four of a size, into one that keeps their places
//	retention.go   whole segments and head rows past the cutoff
//	damage.go      Damaged, Drop: rows that no longer read, reported once
//
//	read.go        Read: admit → check → reserve → fetch → build a page
//	snapshot.go    candidates and their rows from one read transaction
//	spans.go       blocks filed by width, so a read walks the time index near its range
//	filter.go      the rows of a block a query keeps, one column at a time
//	page.go        records merged in event-time order, and where a page ends
//	follow.go      Follow: sealed segments by (segment, row), through the segment holding each
//	cache.go       the blocks and segment rows Follow fetched, kept for the batches after it
//
//	segment.go     a segment: records sorted, cut into blocks, keys listed
//	schema.go      the segment row: names, shapes, contexts; one parser
//	block.go       the block row: columns in slot order; one parser
//	ints.go        integer columns: transform, base and divisor, packer
//	values.go      JSON value columns: integers, quoted integers, hex, uuids, text
//	stamp.go       the times a text spells, kept behind its record's time
//	text.go        text columns and the one zstd pass over a blob
//	bits.go        bit packing and rice codes
//	bloom.go       blooms over trace ids and id-like attributes
//	cursor.go      bounded reads of stored bytes
//	codec.go       the encoder's and decoder's state
package records
