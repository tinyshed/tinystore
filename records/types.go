package records

import (
	"fmt"
	"log/slog"
	"time"
)

// Record is one log line or event. A nil Level or Body is absent, which is not
// the same as an empty body, and a zero TraceID or SpanID is none. Context
// says who produced the record and Attrs what happened; both keep their order
// and repeated keys.
type Record struct {
	At      time.Time // unix nanoseconds in the file, read back in UTC
	Stream  string    // a namespace the application names: a service, a container
	Name    string    // the event: "click", "checkout"; the slog handler writes "log"
	Level   *slog.Level
	Body    *string
	TraceID TraceID
	SpanID  SpanID
	Context []Field
	Attrs   []Field
}

type TraceID [16]byte

type SpanID [8]byte

// RecordError is an Append refused because of one record. Nothing was
// written, so the others may be sent again without it.
type RecordError struct {
	Index        int
	Stream, Name string
	Err          error
}

func (e *RecordError) Error() string {
	return fmt.Sprintf("record %d (stream %q, name %q): %v", e.Index, e.Stream, e.Name, e.Err)
}

func (e *RecordError) Unwrap() error { return e.Err }

// Query selects the records in [From, To) that meet every condition given:
// one of Streams, one of Names, a level of MinLevel or above, the TraceID, and
// each of Attrs and Context by key and exact JSON spelling.
type Query struct {
	From, To time.Time
	Streams  []string
	Names    []string
	MinLevel *slog.Level // a record without a level does not match
	TraceID  TraceID
	Attrs    []Field
	Context  []Field
	Newest   bool   // newest first; oldest first otherwise
	Limit    int    // records a page returns: 1000 when zero, at most 10000
	Budget   Budget // may only narrow Options.Budget
}

// Budget bounds one Read before it starts: the blocks it may open, the bytes
// it may fetch and the records it may decode. Reaching it ends a page early
// rather than failing the read.
type Budget struct {
	Blocks, Bytes, Decoded int
}

// Page is one bounded part of an answer, in event-time order. It never splits
// a timestamp. More says the limit or the budget ended it before the range
// did, and Next is the same query with its range moved past this page.
type Page struct {
	Records []Record
	More    bool
	Next    Query
}

// Cursor is a place in the sealed segments, in the order they were sealed:
// the record at Row of the segment Segment. The zero Cursor is the oldest
// segment still kept.
type Cursor struct {
	Segment int64
	Row     int
}

// Batch is what Follow read. Expired counts the segments retention removed
// before the cursor reached them.
type Batch struct {
	Records []Record
	Next    Cursor
	Expired int
}

// Damage is a row that no longer reads: a head row, or a sealed segment with
// its blocks. Its records are lost; Drop removes it, so that the reads over it
// work again.
type Damage struct {
	Stream   string
	Segment  int64     // a sealed segment, dropped whole; zero for a head row
	HeadRow  int64     // zero for a segment
	From, To time.Time // the times it held
	Reason   string    // the invariant its bytes broke
}

// DamageError is a read, a follow or a seal that met a damaged row. errors.Is
// finds tinystore.ErrCorrupt in it, and its Damage is what to Drop.
type DamageError struct {
	Damage Damage
	Err    error
}

func (e *DamageError) Error() string {
	if e.Damage.Segment != 0 {
		return fmt.Sprintf("segment %d of %q: %v", e.Damage.Segment, e.Damage.Stream, e.Err)
	}
	return fmt.Sprintf("head row %d of %q: %v", e.Damage.HeadRow, e.Damage.Stream, e.Err)
}

func (e *DamageError) Unwrap() error { return e.Err }

// Stats counts this handle's work. Dropped is the sum of its three reasons:
// a full buffer, an invalid or out-of-window record, and a failed flush.
// ReadBlocks and ReadBytes
// are what reads and follows fetched: blocks and head rows, and their bytes
// with the segment rows beside them, the figures a Budget bounds. Damaged is
// how many rows this handle has met that no longer read and are not dropped.
type Stats struct {
	Appended, Dropped                         uint64
	DroppedFull, DroppedInvalid, DroppedWrite uint64
	SealedSegments, ExpiredSegments           uint64
	MergedSegments                            uint64
	Queries                                   uint64
	ReadBlocks, ReadBytes                     uint64
	Damaged                                   uint64
}

// Maintenance is what one Maintain call did. MergedSegments counts the
// segments a merge moved into another segment of their stream, each keeping
// its place for Follow, and MergedRecords the records the merges wrote again.
// Damaged counts the head rows that no longer read, which the rest of their
// heads sealed around.
type Maintenance struct {
	SealedSegments, SealedRecords int
	ExpiredSegments, ExpiredHeads int
	MergedSegments, MergedRecords int
	Damaged                       int
}
