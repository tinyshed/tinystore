package records

import (
	"fmt"
	"log/slog"
	"time"
)

// Record is one log line or event.
type Record struct {
	At     time.Time // unix nanoseconds in the file, read back in UTC
	Stream string    // a namespace the application names: a service, a container
	Name   string    // the event: "click", "checkout"; the slog handler writes "log"
	// Level is absent when nil.
	Level *slog.Level
	// Body is absent when nil, which is not the same as an empty body.
	Body *string
	// TraceID is none when zero.
	TraceID TraceID
	// SpanID is none when zero.
	SpanID SpanID
	// Context says who produced the record. It keeps its order and repeated
	// keys.
	Context []Field
	// Attrs says what happened. It keeps its order and repeated keys.
	Attrs []Field
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

// Query selects the records in [From, To) that meet every condition given.
type Query struct {
	From, To time.Time
	// Streams matches a record of any one of them.
	Streams []string
	// Names matches a record of any one of them.
	Names []string
	// MinLevel matches a level of MinLevel or above. A record without a level
	// does not match.
	MinLevel *slog.Level
	TraceID  TraceID
	// Attrs matches a record holding each field, by key and exact JSON
	// spelling.
	Attrs []Field
	// Context matches a record holding each field, by key and exact JSON
	// spelling.
	Context []Field
	Newest  bool   // newest first; oldest first otherwise
	Limit   int    // records a page returns: 1000 when zero, at most 10000
	Budget  Budget // may only narrow Options.Budget
}

// Budget bounds one Read before it starts: the blocks it may open, the bytes
// it may fetch and the records it may decode. Reaching it ends a page early
// rather than failing the read.
type Budget struct {
	Blocks, Bytes, Decoded int
}

// Page is one bounded part of an answer, in event-time order. It never splits
// a timestamp.
type Page struct {
	Records []Record
	// More says the limit or the budget ended the page before the range did.
	More bool
	// Next is the same query with its range moved past this page.
	Next Query
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

// Stats counts this handle's work.
type Stats struct {
	// Dropped is the sum of the three reasons that follow.
	Appended, Dropped uint64
	// DroppedFull is a full buffer, DroppedInvalid an invalid or
	// out-of-window record, and DroppedWrite a failed flush.
	DroppedFull, DroppedInvalid, DroppedWrite uint64
	SealedSegments, ExpiredSegments           uint64
	MergedSegments                            uint64
	Queries                                   uint64
	// ReadBlocks and ReadBytes are what reads and follows fetched. They count
	// blocks and head rows, and their bytes with the segment rows beside them:
	// the figures a Budget bounds.
	ReadBlocks, ReadBytes uint64
	// Damaged is how many rows this handle has met that no longer read and
	// are not dropped.
	Damaged uint64
}

// Maintenance is what one Maintain call did.
type Maintenance struct {
	SealedSegments, SealedRecords int
	ExpiredSegments, ExpiredHeads int
	// MergedSegments counts the segments a merge moved into another segment of
	// their stream, each keeping its place for Follow. MergedRecords counts
	// the records the merges wrote again.
	MergedSegments, MergedRecords int
	// Damaged counts the head rows that no longer read, which the rest of
	// their heads sealed around.
	Damaged int
}
