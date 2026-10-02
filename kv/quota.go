package kv

import (
	"context"
	"encoding/json"
	"fmt"
	"math"
	"regexp"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// the kind of bucket a quota keeps its keys' windows in
const kindQuota = "quota"

const (
	maxWindows = 8       // the windows one quota counts
	quotaRow   = 2 << 10 // the most a key's row of windows holds, as it is read or written
)

var windowName = regexp.MustCompile(`^[a-z][a-z0-9_]{0,31}$`)

// Quota lets each key use up to a limit in every one of its windows, such as
// 100 messages every five hours and 300 a week, counted by one Allow.
//
// A window starts at a key's first use after the last one ended and lasts its
// span, as a counter with DefaultTTL does, so that each key resets on its own:
//
//	Window("session", 100, 5*time.Hour): first use 13:42 → resets at 18:42;
//	a use at 19:07 starts the next window, which resets at 00:07
//
// An Allow checks every window and counts in each, or in none, in one durable
// write: two quotas asked one after the other would leave the first counted
// when the second refused.
type Quota struct {
	rows    *Bucket[[]byte]
	windows []window
	least   int64 // the smallest limit, past which a use never passes
}

// window is a span in which a key may use up to limit
type window struct {
	name  string
	limit int64
	span  int64 // milliseconds
}

// QuotaUsage is what a quota answers a key: whether its use passed, how many
// more would pass now and, when it did not, how long until it would, as a
// limiter's Allowance says; and each window by its name.
type QuotaUsage struct {
	Allowance
	Windows map[string]WindowUsage
}

// WindowUsage is one window of a key.
type WindowUsage struct {
	Used, Limit, Left int64
	// ResetAt is when the window ends, after which the next use starts
	// another; zero for a window not started.
	ResetAt time.Time
}

// QuotaOption is one of a quota's windows.
type QuotaOption func(*quotaSettings)

type quotaSettings struct {
	windows []window
	err     error
}

// Window lets a key use up to limit every span, from its first use:
// Window("weekly", 300, 7*24*time.Hour). A name is [a-z][a-z0-9_]{0,31}, and
// a quota has one to eight windows.
func Window(name string, limit int64, span time.Duration) QuotaOption {
	return func(s *quotaSettings) {
		w := window{name: name, limit: limit, span: span.Milliseconds()}
		switch {
		case !windowName.MatchString(name):
			s.err = fmt.Errorf("%w: a window name is [a-z][a-z0-9_]{0,31}, not %q", tinystore.ErrInvalid, name)
		case limit < 1 || w.span < 1:
			s.err = fmt.Errorf("%w: window %q of %d every %v, not at least one every millisecond",
				tinystore.ErrInvalid, name, limit, span)
		}
		s.windows = append(s.windows, w)
	}
}

// OpenQuota opens the quota name of kv.db, creating it the first time. A name
// that holds anything else is ErrInvalid. Its windows are the caller's and may
// change between runs: a window kept under a name no longer given is
// forgotten, and a limit changed counts what its window used before.
func OpenQuota(ctx context.Context, state *Store, name string, windows ...QuotaOption) (*Quota, error) {
	said, err := quotaWindows(windows)
	if err != nil {
		return nil, fmt.Errorf("kv: quota %q: %w", name, err)
	}

	id, err := state.claimBucket(ctx, name, kindQuota)
	if err != nil {
		return nil, err
	}

	least := int64(math.MaxInt64)
	for _, w := range said {
		least = min(least, w.limit)
	}
	opened := branch{state: state, id: id, name: name}
	return &Quota{rows: &Bucket[[]byte]{branch: opened, codec: codecFor[[]byte]()}, windows: said, least: least}, nil
}

// quotaWindows is a quota's windows, refused when there are none, too many,
// or two of one name
func quotaWindows(options []QuotaOption) ([]window, error) {
	var said quotaSettings
	for _, option := range options {
		option(&said)
	}
	if said.err != nil {
		return nil, said.err
	}
	if len(said.windows) < 1 || len(said.windows) > maxWindows {
		return nil, fmt.Errorf("%w: %d windows, not one to %d", tinystore.ErrInvalid, len(said.windows), maxWindows)
	}
	seen := map[string]bool{}
	for _, w := range said.windows {
		if seen[w.name] {
			return nil, fmt.Errorf("%w: two windows named %q", tinystore.ErrInvalid, w.name)
		}
		seen[w.name] = true
	}
	return said.windows, nil
}

// Of is the branch of this quota that owners name, as Bucket.Of names one.
func (q *Quota) Of(owners ...any) *Quota {
	return &Quota{rows: q.rows.Of(owners...), windows: q.windows, least: q.least}
}

// Allow uses one of key's every window, or none when one has no room left.
func (q *Quota) Allow(ctx context.Context, key any) (QuotaUsage, error) {
	return q.AllowN(ctx, key, 1)
}

// AllowN uses n of key's every window at once, or none when one has no room
// for n. More than a window's limit never passes, and is ErrInvalid.
func (q *Quota) AllowN(ctx context.Context, key any, n int64) (QuotaUsage, error) {
	c, err := q.rows.begin(key, nil)
	if err == nil && (n < 1 || n > q.least) {
		err = fmt.Errorf("%w: %d at once, past the smallest window's limit of %d", tinystore.ErrInvalid, n, q.least)
	}
	if err != nil {
		return QuotaUsage{}, q.rows.fail(c, err)
	}

	var usage QuotaUsage
	err = q.rows.write(ctx, quotaRow, func(w sqlite.Writer) error {
		held, readErr := q.held(ctx, w, c)
		if readErr != nil {
			return readErr
		}
		var next windowsHeld
		next, usage = q.decide(held, c.now, n)
		if !usage.OK {
			return nil
		}
		return q.writeWindows(ctx, w, c, next)
	})
	return usage, q.rows.fail(c, err)
}

// Get is key's windows without using them: OK says whether one more use would
// pass now.
func (q *Quota) Get(ctx context.Context, key any) (QuotaUsage, error) {
	c, err := q.rows.begin(key, nil)
	if err != nil {
		return QuotaUsage{}, q.rows.fail(c, err)
	}
	reserved, err := q.rows.reserve(ctx, quotaRow)
	if err != nil {
		return QuotaUsage{}, q.rows.fail(c, err)
	}
	defer reserved.Release()

	var held windowsHeld
	err = q.rows.read(ctx, func(r sqlite.Reader) (readErr error) {
		held, readErr = q.held(ctx, r, c)
		return readErr
	})
	if err != nil {
		return QuotaUsage{}, q.rows.fail(c, err)
	}
	_, usage := q.decide(held, c.now, 0)
	return usage, nil
}

// Refund gives one use back to each of key's windows that has not reset
// since; a window never goes below nothing.
func (q *Quota) Refund(ctx context.Context, key any) error {
	return q.RefundN(ctx, key, 1)
}

// RefundN gives n uses back, as Refund gives one.
func (q *Quota) RefundN(ctx context.Context, key any, n int64) error {
	c, err := q.rows.begin(key, nil)
	if err == nil && n < 1 {
		err = fmt.Errorf("%w: a refund of %d", tinystore.ErrInvalid, n)
	}
	if err != nil {
		return q.rows.fail(c, err)
	}

	err = q.rows.write(ctx, quotaRow, func(w sqlite.Writer) error {
		held, readErr := q.held(ctx, w, c)
		if readErr != nil {
			return readErr
		}
		next := windowsHeld{}
		for _, win := range q.windows {
			if kept, ok := held[win.name]; ok && kept[1] > c.now {
				next[win.name] = [2]int64{max(kept[0]-n, 0), kept[1]}
			}
		}
		if len(next) == 0 {
			return nil
		}
		return q.writeWindows(ctx, w, c, next)
	})
	return q.rows.fail(c, err)
}

// Delete forgets key's windows, so that its next use starts each anew.
func (q *Quota) Delete(ctx context.Context, key any) error {
	return q.rows.Delete(ctx, key)
}

// windowsHeld is what a key's row keeps: each window's use and when it resets,
// in unix milliseconds, by the window's name
type windowsHeld map[string][2]int64

// decide is the algorithm. A window whose reset has come is one not started;
// n passes when every window has room for it, and is then counted in each, a
// window not started starting now. A Get asks with n of 0, whether one more
// would pass, and counts nothing.
//
//	session 100 a 5h, weekly 300 a 7d; used 37 and 300 → weekly refuses: nothing
//	is counted, and RetryAfter is weekly's reset
func (q *Quota) decide(held windowsHeld, now, n int64) (windowsHeld, QuotaUsage) {
	asked := max(n, 1)
	passes, wait := true, int64(0)
	next := make(windowsHeld, len(q.windows))
	for _, w := range q.windows {
		kept := held[w.name]
		if kept[1] <= now {
			kept = [2]int64{}
		}
		if kept[0]+asked > w.limit {
			passes, wait = false, max(wait, kept[1]-now)
		}
		next[w.name] = kept
	}

	usage := QuotaUsage{
		Allowance: Allowance{OK: passes, Left: math.MaxInt64}, Windows: make(map[string]WindowUsage, len(q.windows)),
	}
	for _, w := range q.windows {
		kept := next[w.name]
		if passes && n > 0 {
			if kept[1] == 0 {
				kept[1] = now + w.span
			}
			kept[0] += n
			next[w.name] = kept
		}
		left := max(w.limit-kept[0], 0)
		usage.Left = min(usage.Left, left)
		usage.Windows[w.name] = WindowUsage{
			Used: kept[0], Limit: w.limit, Left: left, ResetAt: expiryTime(kept[1], kept[1] != 0),
		}
	}
	if !passes {
		usage.RetryAfter = time.Duration(wait) * time.Millisecond
	}
	return next, usage
}

// held is what key's row keeps of its windows, nil for a key not used or one
// whose windows have all ended
func (q *Quota) held(ctx context.Context, r sqlite.Reader, c call) (windowsHeld, error) {
	there, found, err := readLive(ctx, r, q.rows.id, c)
	if err != nil || !found {
		return nil, err
	}
	value, err := there.held()
	if err != nil {
		return nil, err
	}
	var held windowsHeld
	encoded, isBytes := value.([]byte)
	if !isBytes || json.Unmarshal(encoded, &held) != nil {
		return nil, fmt.Errorf("%w: a quota's row is not the windows it wrote", tinystore.ErrCorrupt)
	}
	return held, nil
}

// writeWindows writes key's windows, the row expiring as the last of them resets
func (q *Quota) writeWindows(ctx context.Context, w sqlite.Writer, c call, next windowsHeld) error {
	encoded, err := json.Marshal(next)
	if err != nil {
		return err
	}
	value, err := keep(encoded)
	if err != nil {
		return err
	}
	last := int64(0)
	for _, kept := range next {
		last = max(last, kept[1])
	}
	c.options.expireAt = time.UnixMilli(last)
	_, err = q.rows.setCell(ctx, w, c, value)
	return err
}
