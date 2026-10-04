package blobs

import (
	"fmt"
	"regexp"
	"time"

	"github.com/tinyshed/tinystore"
)

// Options changes how the engine keeps its files.
type Options struct {
	// KeepFree is the disk an upload leaves free, so that one upload cannot
	// take the space every engine's commits need. It is 1 GiB when zero, and
	// nothing is checked when it is negative.
	KeepFree int64
}

// the engine's own bounds and schedule
// LimitObjectBytes is the name a LimitError holds for an object past its bucket's MaxSize.
const LimitObjectBytes = "bytes of an object, the bucket's MaxSize"

const (
	pageSize        = 4 << 10     // an object's row is its path, a few numbers, its type and its meta
	readers         = 8           // the reader connections: lookups, scans and counts
	writeSlots      = 2048        // writes at once: a group of 1024 gathering while one commits
	uploadSlots     = 1024        // uploads at once; past them an upload waits for a slot
	inlineSize      = 16 << 10    // a content up to it is a row of bodies: the round's crossing on both systems
	streamBuffer    = 64 << 10    // a long Put can carry more bytes when the memory is free immediately
	streamAfter     = 256 << 10   // small files keep the initial buffer
	idBlock         = 1000        // content ids reserved in one write of meta
	idsPerDir       = 12          // bits of an id that name its file within its directory: 4,096 a directory
	syncEvery       = 256 << 20   // an upload's bytes synced this often, so that no commit waits on gigabytes
	freeEvery       = 64 << 20    // an upload checks the disk's free space this often
	defaultKeepFree = 1 << 30     // what an upload leaves free on the disk unless Options say
	maxPath         = 1 << 10     // a path's bytes, owners and key
	maxSegments     = 16          // a path's segments, owners and key
	maxType         = 256         // a content type's bytes
	maxMeta         = 2 << 10     // meta's keys and values together
	objectHeld      = 4 << 10     // what an object read by a Scan may hold: its path, type and meta
	scanLimit       = 100         // objects a page returns when a query does not say
	maxScanLimit    = 1000        // objects a page may return
	clearAtOnce     = 10_000      // objects a Clear deletes in its transaction; past them it marks
	maintainEvery   = time.Minute // how often maintenance runs
	maintainBatch   = 10_000      // rows one maintenance transaction removes
	maintainBatches = 10          // transactions of each kind one Maintain runs at most
	openLookups     = 3           // lookups an Open makes while its key changes under it
	renameTries     = 5           // tries of a rename a scanner holds on Windows
	scrubPass       = 30 * 24 * time.Hour
	scrubBuffer     = 1 << 20 // what the scrub reads at once, and holds of the store's memory
	quietFailures   = 10 * time.Minute
)

// a bucket's name and a meta key, as a file's name is: short and plain
var validName = regexp.MustCompile(`^[a-z0-9][a-z0-9_-]{0,63}$`)

// BucketOption changes how a bucket treats its objects; none changes what its
// rows mean, so a program may change them between runs.
type BucketOption func(*bucketSettings)

type bucketSettings struct {
	ttl     time.Duration
	maxSize int64
	err     error
}

// DefaultTTL is the expiry a write gives an object when it names none, d from
// the write.
func DefaultTTL(d time.Duration) BucketOption {
	return func(s *bucketSettings) {
		if d <= 0 {
			s.err = fmt.Errorf("%w: blobs: a default TTL of %v", tinystore.ErrInvalid, d)
		}
		s.ttl = d
	}
}

// MaxSize bounds the bucket's objects: the upload that passes n bytes stops
// with tinystore.ErrLimit and leaves nothing. There is no bound unless it says.
func MaxSize(n int64) BucketOption {
	return func(s *bucketSettings) {
		if n <= 0 {
			s.err = fmt.Errorf("%w: blobs: MaxSize(%d)", tinystore.ErrInvalid, n)
		}
		s.maxSize = n
	}
}

func collectBucket(options []BucketOption) (bucketSettings, error) {
	var s bucketSettings
	for _, option := range options {
		option(&s)
	}
	return s, s.err
}

// Option changes one call. Put and Create take every option; Copy and Move
// all but Size; Delete only IfMatch. Any other is tinystore.ErrInvalid.
type Option func(*callSettings)

// callSettings is what a call's options say; size is -1 when none says
type callSettings struct {
	contentType string
	typed       bool
	meta        map[string]string
	ttl         time.Duration
	expireAt    time.Time
	size        int64
	ifMatch     string
	ifNoneMatch bool
	given       []string // the options' names, so that a call refuses one it does not take
	err         error
}

// ContentType is the object's type, kept as it is given, up to 256 bytes.
func ContentType(t string) Option {
	return func(s *callSettings) {
		s.note("ContentType")
		if len(t) > maxType {
			s.fail("a content type of %d bytes, over %d", len(t), maxType)
		}
		s.contentType, s.typed = t, true
	}
}

// Meta adds a string for serving the object, a file's original name or a
// width: a key is [a-z0-9][a-z0-9_-]{0,63}, and keys and values together are
// at most 2 KiB. A write that names Meta replaces all of it.
func Meta(key, value string) Option {
	return func(s *callSettings) {
		s.note("Meta")
		if !validName.MatchString(key) {
			s.fail("a meta key is [a-z0-9][a-z0-9_-]{0,63}, not %q", key)
		}
		if s.meta == nil {
			s.meta = map[string]string{}
		}
		s.meta[key] = value
	}
}

// TTL gives the object d from its write.
func TTL(d time.Duration) Option {
	return func(s *callSettings) {
		s.note("TTL")
		if d <= 0 {
			s.fail("a TTL of %v", d)
		}
		s.ttl = d
	}
}

// ExpireAt gives the object until t by the store's clock.
func ExpireAt(t time.Time) Option {
	return func(s *callSettings) {
		s.note("ExpireAt")
		if t.IsZero() {
			s.fail("a zero expiry")
		}
		s.expireAt = t
	}
}

// Size says the stream's length, so that the engine places the object before
// its first byte; a stream shorter or longer is tinystore.ErrInvalid and leaves
// nothing. A negative n says nothing, so Size(r.ContentLength) needs no
// condition around it.
func Size(n int64) Option {
	return func(s *callSettings) {
		s.note("Size")
		s.size = max(n, -1)
	}
}

// IfMatch writes or deletes only while the key holds a live object with this
// ETag, as an HTTP If-Match header spells it: a list of ETags, any of them, or
// * for any live object. Any other version, and an absent key, is
// tinystore.ErrConflict.
func IfMatch(etag string) Option {
	return func(s *callSettings) {
		s.note("IfMatch")
		if etag == "" {
			s.fail("IfMatch with no ETag")
		}
		s.ifMatch = etag
	}
}

// IfNoneMatch writes only while the key holds no live object, so that an
// upload retried after a timeout adds nothing.
func IfNoneMatch() Option {
	return func(s *callSettings) {
		s.note("IfNoneMatch")
		s.ifNoneMatch = true
	}
}

func (s *callSettings) note(option string) {
	s.given = append(s.given, option)
}

func (s *callSettings) fail(format string, args ...any) {
	if s.err == nil {
		s.err = fmt.Errorf("%w: "+format, append([]any{tinystore.ErrInvalid}, args...)...)
	}
}

// what a kind of call takes, by the names of its options
var (
	putTakes    = takes("ContentType", "Meta", "TTL", "ExpireAt", "Size", "IfMatch", "IfNoneMatch")
	copyTakes   = takes("ContentType", "Meta", "TTL", "ExpireAt", "IfMatch", "IfNoneMatch")
	deleteTakes = takes("IfMatch")
)

func takes(options ...string) map[string]bool {
	taken := make(map[string]bool, len(options))
	for _, option := range options {
		taken[option] = true
	}
	return taken
}

// collect reads a call's options, and refuses one its call does not take,
// both TTL and ExpireAt, both conditions, and meta over its bound
func collect(options []Option, call string, takes map[string]bool) (callSettings, error) {
	s := callSettings{size: -1}
	for _, option := range options {
		option(&s)
	}
	for _, name := range s.given {
		if !takes[name] {
			s.fail("%s takes no %s", call, name)
		}
	}
	switch {
	case s.ttl > 0 && !s.expireAt.IsZero():
		s.fail("both TTL and ExpireAt")
	case s.ifMatch != "" && s.ifNoneMatch:
		s.fail("both IfMatch and IfNoneMatch")
	case metaBytes(s.meta) > maxMeta:
		s.fail("meta of %d bytes, over %d", metaBytes(s.meta), maxMeta)
	}
	return s, s.err
}

func metaBytes(meta map[string]string) int {
	total := 0
	for key, value := range meta {
		total += len(key) + len(value)
	}
	return total
}

// expires is the expiry a write gives its object in unix milliseconds: the
// call's, else the bucket's default from now, else none
func (s callSettings) expires(now int64, defaultTTL time.Duration) (int64, bool) {
	switch {
	case s.ttl > 0:
		return now + s.ttl.Milliseconds(), true
	case !s.expireAt.IsZero():
		return s.expireAt.UnixMilli(), true
	case defaultTTL > 0:
		return now + defaultTTL.Milliseconds(), true
	}
	return 0, false
}
