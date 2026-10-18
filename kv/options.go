package kv

import (
	"time"
)

// Each call takes the options it has a use for, and a call given one it has
// none for does not compile: kv.Idle opens a bucket and never sets a key.

// BucketOption is how a bucket opens.
type BucketOption interface{ bucket(*bucketOptions) }

// CountersOption is how counters open.
type CountersOption interface{ counters(*countersOptions) }

// SetOption is what a Set gives besides its value: an expiry, or the version
// the key must still be at.
type SetOption interface{ set(*writeOptions) }

// CreateOption is what a Create or an Add gives besides its value: an expiry.
type CreateOption interface{ create(*writeOptions) }

// CheckOption is the version a Take, a Delete or an Expire needs the key at.
type CheckOption interface{ check(*writeOptions) }

// ListOption is how a page of keys reads.
type ListOption interface{ list(*listOptions) }

// OnceOption is how answers kept once a key open.
type OnceOption interface{ once(*onceOptions) }

// RateLimitOption is how a rate limit opens.
type RateLimitOption interface{ rateLimit(*rateLimitOptions) }

// QuotaOption is how a quota opens.
type QuotaOption interface{ quota(*quotaOptions) }

type bucketOptions struct {
	ttl, idle *time.Duration
	encrypted bool
}

type countersOptions struct {
	ttl, flushEvery *time.Duration
}

type writeOptions struct {
	ttl       *time.Duration
	expiresAt *time.Time
	ifVersion Version
	checked   bool
}

type listOptions struct {
	limit *uint64
	after *string
}

type onceOptions struct{ keep *time.Duration }

type rateLimitOptions struct {
	rate  uint64
	per   time.Duration
	burst *uint64
}

type quotaOptions struct{ windows []window }

type window struct {
	name  string
	limit uint64
	per   time.Duration
}

type ttl time.Duration

// TTL is how long a key lives from its last write: a bucket's or counters'
// for every key, or one write's own.
func TTL(d time.Duration) ttl { return ttl(d) }

func (t ttl) bucket(o *bucketOptions)     { d := time.Duration(t); o.ttl = &d }
func (t ttl) counters(o *countersOptions) { d := time.Duration(t); o.ttl = &d }
func (t ttl) set(o *writeOptions)         { d := time.Duration(t); o.ttl = &d }
func (t ttl) create(o *writeOptions)      { d := time.Duration(t); o.ttl = &d }

type idle time.Duration

// Idle is how long a key lives from its last read or write: a session gone
// unused. A bucket has a TTL or an Idle, not both.
func Idle(d time.Duration) idle { return idle(d) }

func (i idle) bucket(o *bucketOptions) { d := time.Duration(i); o.idle = &d }

type encrypted struct{}

// Encrypted keeps every value of the bucket sealed with the store's encryption key.
func Encrypted() encrypted { return encrypted{} }

func (encrypted) bucket(o *bucketOptions) { o.encrypted = true }

type flushEvery time.Duration

// FlushEvery keeps counts in memory and writes them that often, so that an
// add costs no commit and a crash loses at most that span.
func FlushEvery(d time.Duration) flushEvery { return flushEvery(d) }

func (f flushEvery) counters(o *countersOptions) { d := time.Duration(f); o.flushEvery = &d }

type expiresAt time.Time

// ExpiresAt is when a key a write gives expires, as a time.
func ExpiresAt(t time.Time) expiresAt { return expiresAt(t) }

func (e expiresAt) set(o *writeOptions)    { t := time.Time(e); o.expiresAt = &t }
func (e expiresAt) create(o *writeOptions) { t := time.Time(e); o.expiresAt = &t }

type ifVersion Version

// IfVersion writes only while the key is at this version, an entry's
// Version: tinystore.ErrConflict otherwise.
func IfVersion(v Version) ifVersion { return ifVersion(v) }

func (v ifVersion) set(o *writeOptions)   { o.ifVersion, o.checked = Version(v), true }
func (v ifVersion) check(o *writeOptions) { o.ifVersion, o.checked = Version(v), true }

type limit uint64

// Limit is how many keys a page holds: 1000, and 10,000 at most.
func Limit(n int) limit { return limit(max(n, 0)) }

func (l limit) list(o *listOptions) { n := uint64(l); o.limit = &n }

type after string

// After starts a page after a key: the page before's Next.
func After(key string) after { return after(key) }

func (a after) list(o *listOptions) {
	if a != "" {
		key := string(a)
		o.after = &key
	}
}

type keep time.Duration

// Keep is how long an answer is kept: a day unless given.
func Keep(d time.Duration) keep { return keep(d) }

func (k keep) once(o *onceOptions) { d := time.Duration(k); o.keep = &d }

type rate struct {
	n   int
	per time.Duration
}

// Rate is a rate limit's smooth rate: n requests a per, kv.Rate(100, time.Second).
func Rate(n int, per time.Duration) rate { return rate{n, per} }

func (r rate) rateLimit(o *rateLimitOptions) { o.rate, o.per = uint64(max(r.n, 0)), r.per }

type burst int

// Burst is how many requests a rate limit lets through at once.
func Burst(n int) burst { return burst(n) }

func (b burst) rateLimit(o *rateLimitOptions) { n := uint64(max(int(b), 0)); o.burst = &n }

type windowOption window

// Window is a quota's window: n uses a per, counted from a key's first use,
// kv.Window("daily", 100, 24*time.Hour).
func Window(name string, n int, per time.Duration) windowOption {
	return windowOption{name, uint64(max(n, 0)), per}
}

func (w windowOption) quota(o *quotaOptions) { o.windows = append(o.windows, window(w)) }
