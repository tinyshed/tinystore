package records

import (
	"fmt"
	"io"
	"log/slog"
	"time"

	"github.com/tinyshed/tinystore"
)

type Options struct {
	// Retention is how long a record is kept: a segment goes once its newest
	// record is older, and Append refuses an older one. Fourteen days when
	// zero.
	Retention time.Duration

	// ClockSkew is how far ahead of the store's clock a record's time may be;
	// Append refuses a later one, which would hold its segment past retention.
	// Ten minutes when zero.
	ClockSkew time.Duration

	// SealAge is how long a quiet stream's records wait in the head before
	// they are sealed however few they are, and so how far Follow may lag
	// behind them: longer means fewer, larger segments. An hour when zero.
	SealAge time.Duration

	// Buffer is how many records the handler holds in memory; beyond it a
	// record is dropped and counted, never waited for. 1024 when zero.
	Buffer int

	// Flush is how often the handler writes what it holds, and sooner once
	// half its Buffer waits. A second when zero.
	Flush time.Duration

	// Budget is the most one Read may spend; a query may only narrow it.
	Budget Budget
}

// HandlerOption changes what a Handler does beside queueing its lines.
type HandlerOption interface{ handlerOption(*handlerSettings) }

type handlerSettings struct {
	console Console
	stdout  bool
	redact  []string
	level   slog.Leveler
	out     io.Writer // a test's, in place of the process's own stream
}

// Console says how a Handler writes its lines as they are logged. Without one
// they are pretty on a terminal and a JSON line otherwise.
type Console int

const (
	// ConsolePretty writes a line for a person: 11:02:11.123 INFO  api  started  port=3000
	ConsolePretty Console = iota + 1
	// ConsoleJSON writes one JSON object a line, for a collector.
	ConsoleJSON
	// ConsoleOff writes nothing: the lines go to the store alone.
	ConsoleOff
)

func (c Console) handlerOption(s *handlerSettings) { s.console = c }

// Stdout writes a Handler's console lines to standard output. They go to
// standard error otherwise, where they never mix with what the program prints:
// a command's answer, or the frames of a protocol spoken over stdout.
const Stdout = toStdout(true)

type toStdout bool

func (t toStdout) handlerOption(s *handlerSettings) { s.stdout = bool(t) }

// Redact hides the values of fields with these names, in the store and on the
// console: a field whose key, or the part of a dotted key after its last dot,
// is one of them, the case ignored, and such a key at any depth of a JSON
// object. A line's message is not searched.
func Redact(names ...string) HandlerOption {
	return handlerFunc(func(s *handlerSettings) { s.redact = append(s.redact, names...) })
}

// Level keeps a Handler's lines from level up, in the store and on the
// console; without it every line is kept.
func Level(level slog.Leveler) HandlerOption {
	return handlerFunc(func(s *handlerSettings) { s.level = level })
}

type handlerFunc func(*handlerSettings)

func (f handlerFunc) handlerOption(s *handlerSettings) { f(s) }

// the bounds of research's design/records.md: every density and memory figure
// the design rests on was measured with them, so they are the format's, not
// options
const (
	maxSegmentRecords = 16_384
	maxSegmentInput   = 4 << 20
	maxBlockRecords   = 1024
	maxBlockInput     = 256 << 10
	maxFields         = 128     // fields in one context or one list of attributes
	maxTypedShapes    = 64      // shapes a segment gives columns; past them attributes are serialized
	maxShapes         = 128     // shapes a segment row holds, serialized ones included
	maxAttrColumns    = 1024    // attribute columns in a segment
	maxContextCells   = 1 << 20 // context values in a segment
	maxIntDictionary  = 256     // values an integer dictionary holds
	maxExpandedText   = 4 << 20 // text one block may decompress or copy
	// what a segment row may decompress or copy: its contexts come from at most
	// maxSegmentInput of records
	maxSegmentExpansion = 2 * maxSegmentInput
	maxAppendInput      = maxSegmentInput // what one Append may carry, whatever the store's memory
)

// what the engine does on its own schedule, and how long it waits
const (
	lateness           = 10 * time.Second // behind its stream's newest before it, a record goes to the late head
	maintenanceEvery   = time.Minute      // seal and expire
	snapshotTimeout    = 5 * time.Second  // the longest a read holds its snapshot
	segmentReservation = 24 << 20         // what encoding or decoding one segment allocates at most
	readSlots          = 2                // reads and follows at once, decoding included: the reader connections
	appendSlots        = 2                // appends at once: one encodes while another writes
	recordReservation  = 1 << 10          // what one decoded record holds
	blockReservation   = 2 << 20          // what decoding one block allocates at most
	defaultLimit       = 1000             // records a page returns when a query does not say
	maxLimit           = 10_000           // records a page may return
	maxLevel           = 1<<31 - 1        // levels travel as 32-bit integers on every platform
	minLevel           = -1 << 31
)

func normalizeOptions(o Options) (Options, error) {
	if o.Retention < 0 || o.ClockSkew < 0 || o.SealAge < 0 || o.Buffer < 0 || o.Flush < 0 {
		return o, fmt.Errorf("%w: records options may not be negative", tinystore.ErrInvalid)
	}
	if o.Budget.Blocks < 0 || o.Budget.Bytes < 0 || o.Budget.Decoded < 0 {
		return o, fmt.Errorf("%w: records budget may not be negative", tinystore.ErrInvalid)
	}
	o.Retention = orDefault(o.Retention, 14*24*time.Hour)
	o.ClockSkew = orDefault(o.ClockSkew, 10*time.Minute)
	o.SealAge = orDefault(o.SealAge, time.Hour)
	o.Buffer = orDefault(o.Buffer, 1024)
	o.Flush = orDefault(o.Flush, time.Second)
	o.Budget.Blocks = orDefault(o.Budget.Blocks, 1024)
	o.Budget.Bytes = orDefault(o.Budget.Bytes, 16<<20)
	o.Budget.Decoded = orDefault(o.Budget.Decoded, 1<<20)
	return o, nil
}

func orDefault[T int | time.Duration](value, fallback T) T {
	if value == 0 {
		return fallback
	}
	return value
}

// narrow applies a query's budget, which may only lower the store's
func (b Budget) narrow(query Budget) (Budget, error) {
	narrowed := b
	fields := []struct {
		asked int
		into  *int
	}{{query.Blocks, &narrowed.Blocks}, {query.Bytes, &narrowed.Bytes}, {query.Decoded, &narrowed.Decoded}}
	for _, field := range fields {
		if field.asked < 0 || field.asked > *field.into {
			return b, fmt.Errorf("%w: a query budget may only narrow the store's %+v", tinystore.ErrInvalid, b)
		}
		if field.asked > 0 {
			*field.into = field.asked
		}
	}
	return narrowed, nil
}
