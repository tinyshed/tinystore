package kv

import (
	"fmt"
	"regexp"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

type Options struct {
	// In keeps the store's buckets in a database's own file, an sqldb DB's,
	// instead of kv.db, so that a batch of the database writes keys with its
	// rows: see Bucket.Written. The database opens before the store and closes
	// after it, and holds one kv store.
	In Database
}

// Database is a file whose owner lets a kv store live in it, as sqldb's DB
// does.
type Database interface {
	SQLiteFile() *sqlite.File
}

const (
	pageSize      = 4 << 10         // at 1 KiB a row with a 256-byte value overflows its page
	readers       = 8               // the reader connections, what the measured reads went through
	writerCache   = 4 << 20         // the writer's pages: 20,000 keys overwritten at random fit, 21 % more Sets
	writeSlots    = 2048            // writes at once: a group of 1024 gathering while one commits
	viewTimeout   = 5 * time.Second // the longest a View holds its snapshot
	expiryEvery   = time.Minute     // how often maintenance deletes expired keys
	expiryBatch   = 10_000          // expired keys a transaction deletes, about 45 ms of the writer
	expiryBatches = 10              // transactions one Maintain runs at most
	scanLimit     = 100             // keys a page returns when a query does not say
	maxScanLimit  = 1000            // keys a page may return
	scanBytes     = 4 << 20         // value bytes a page may hold
	maxHeld       = 100_000         // LoseAtMost counters memory holds, changed or not: about 1 s of flushing
	flushBatch    = 10_000          // counters a flush writes a transaction, 45 to 92 ms of the writer
	clearAtOnce   = 10_000          // keys a Clear deletes in its transaction, 35 to 52 ms; past them it marks
	refreshes     = 30              // a Sliding key is renewed once a thirtieth of its term has passed
	renewAtOnce   = time.Minute     // a Sliding key this close to its expiry is renewed before its read returns
	renewEvery    = time.Second     // how often the renewals reads asked for are written
	maxRenewals   = 100_000         // renewals that may wait; past them a key's next read asks again
)

// catchUpEvery is how soon a Maintain that stopped at its bounds runs again:
// ten batches every ten seconds rather than every minute. That is up to
// 600,000 keys a minute for about 5 % of the writer.
const catchUpEvery = 10 * time.Second

var validName = regexp.MustCompile(`^[a-z0-9][a-z0-9_-]{0,63}$`)

// BucketOption changes how a bucket serves its values; none changes what its
// bytes mean, so a program may change them between runs.
type BucketOption interface{ bucketOption(*settings) }

type CounterOption interface{ counterOption(*settings) }

type OpenOption interface {
	BucketOption
	CounterOption
}

type settings struct {
	ttl        time.Duration
	sliding    time.Duration
	codec      any
	loseAtMost time.Duration
	err        error
}

// an option of one kind only, so that the compiler refuses it for the other,
// and one of both
type (
	forBuckets  func(*settings)
	forCounters func(*settings)
	forBoth     func(*settings)
)

func (f forBuckets) bucketOption(s *settings)   { f(s) }
func (f forCounters) counterOption(s *settings) { f(s) }
func (f forBoth) bucketOption(s *settings)      { f(s) }
func (f forBoth) counterOption(s *settings)     { f(s) }

// LoseAtMost keeps the counters in memory and writes what changed every d, on
// Close and on Maintain, so that an Add takes no write.
//
// A crash loses the changes since the last flush that committed, a Delete's as
// an Add's. A flush waits for the writer as any write does, so on a busy file
// the loss can reach d plus the flush's own wait and time.
//
// Handles on counters of one name share the memory and join no transaction.
// Opening them again with another d, or without LoseAtMost, is ErrInvalid.
func LoseAtMost(d time.Duration) CounterOption {
	return forCounters(func(s *settings) {
		if d <= 0 {
			s.err = fmt.Errorf("%w: kv: LoseAtMost(%v)", tinystore.ErrInvalid, d)
		}
		s.loseAtMost = d
	})
}

// DefaultTTL is the expiry a key gets when it is created without kv.TTL or
// kv.ExpireAt; a later Set or Add keeps the expiry a key has.
func DefaultTTL(d time.Duration) OpenOption {
	return forBoth(func(s *settings) {
		if d <= 0 {
			s.err = fmt.Errorf("%w: kv: a default TTL of %v", tinystore.ErrInvalid, d)
		}
		s.ttl = d
	})
}

// Sliding gives a key term from the last time it was read: a key created
// without kv.TTL or kv.ExpireAt gets term, and a Get, GetEntry or Has renews a
// live key to term from now once a thirtieth of the term has passed since it
// last did. With DefaultTTL it is ErrInvalid.
//
// A read does not write: the renewal waits a second for the next flush, and a
// crash forgets the renewals since the last. So a key read at t lives at least
// until t + term − term/30 unless it is renewed.
//
// A key with less than a minute left is renewed before its read returns.
func Sliding(term time.Duration) BucketOption {
	return forBuckets(func(s *settings) {
		if term <= 0 {
			s.err = fmt.Errorf("%w: kv: Sliding(%v)", tinystore.ErrInvalid, term)
		}
		s.sliding = term
	})
}

// WithCodec writes a bucket's values through codec instead of the bytes their
// type would get; a codec of another value type is ErrInvalid at OpenBucket.
func WithCodec[V any](codec Codec[V]) BucketOption {
	return forBuckets(func(s *settings) { s.codec = codec })
}

type Option func(*callOptions)

type callOptions struct {
	ttl      time.Duration
	expireAt time.Time
	version  Version
	err      error
}

// TTL gives the key d from now; see the rules of DefaultTTL.
func TTL(d time.Duration) Option {
	return func(o *callOptions) {
		if d <= 0 {
			o.err = fmt.Errorf("%w: a TTL of %v", tinystore.ErrInvalid, d)
		}
		o.ttl = d
	}
}

// ExpireAt gives the key until t by the store's clock.
func ExpireAt(t time.Time) Option {
	return func(o *callOptions) {
		if t.IsZero() {
			o.err = fmt.Errorf("%w: a zero expiry", tinystore.ErrInvalid)
		}
		o.expireAt = t
	}
}

// IfVersion lets a call write only to a live key whose version is v; any other
// is tinystore.ErrConflict, an absent or expired key included.
func IfVersion(v Version) Option {
	return func(o *callOptions) {
		if v.revision <= 0 {
			o.err = fmt.Errorf("%w: IfVersion with no version", tinystore.ErrInvalid)
		}
		o.version = v
	}
}

func collect(options []Option) (callOptions, error) {
	if len(options) == 0 {
		return callOptions{}, nil
	}
	var collected callOptions
	for _, option := range options {
		option(&collected)
	}
	if collected.ttl > 0 && !collected.expireAt.IsZero() {
		collected.err = fmt.Errorf("%w: both TTL and ExpireAt", tinystore.ErrInvalid)
	}
	return collected, collected.err
}

func (o callOptions) hasExpiry() bool {
	return o.ttl > 0 || !o.expireAt.IsZero()
}

// expiry is the call's own expiry in unix milliseconds
func (o callOptions) expiry(now int64) int64 {
	if o.ttl > 0 {
		return now + o.ttl.Milliseconds()
	}
	return o.expireAt.UnixMilli()
}
