package jobs

import (
	"fmt"
	"hash/fnv"
	"math"
	"math/bits"
	"strconv"
	"strings"
	"time"

	"github.com/tinyshed/tinystore"
)

// Repeat is when a job runs again: a cron expression in a named zone, or an
// interval. It is kept as text, so that another language reads the same
// schedule:
//
//	jobs.Daily("03:10", moscow)                          10 3 * * * Europe/Moscow
//	jobs.Cron("*/15 9-18 * * 1-5", tz)                   */15 9-18 * * 1-5 America/New_York
//	jobs.Every(15 * time.Minute)                         @every 15m
//	jobs.Every(30*time.Second, jobs.Spread()), probe:7   @every 30s +6178ms
type Repeat struct {
	text   string
	every  time.Duration
	phase  time.Duration // how long after each multiple of every it runs
	spread bool          // the phase is its job's key's, taken when the job keeps it
	cron   cron
	err    error
}

// String is the repeat as jobs.db keeps it; a Spread interval's phase comes
// with its job's key.
func (r Repeat) String() string {
	return r.text
}

func (r Repeat) enqueueOption(s *enqueueSettings) {
	if r.err != nil {
		s.err = r.err
	}
	s.repeat = &r
}

// Cron repeats a job by a five-field cron expression in zone: minute, hour, day
// of the month, month and day of the week. A field takes *, ranges, steps,
// lists and the names of months and days, or the whole expression is @hourly,
// @daily, @weekly, @monthly or @yearly. A day of the month and a day of the
// week both given run on either.
//
// A zone without a name, time.Local, is ErrInvalid, since a schedule kept by it
// would change meaning on a host in another zone.
func Cron(expr string, zone *time.Location) Repeat {
	if len(expr) > maxRepeat {
		return Repeat{err: fmt.Errorf("%w: jobs: repeat exceeds %d bytes", tinystore.ErrLimit, maxRepeat)}
	}
	fields, err := parseCron(expr)
	if err == nil {
		err = checkZone(zone)
	}
	if err != nil {
		return Repeat{err: fmt.Errorf("%w: jobs: repeat %q: %w", tinystore.ErrInvalid, expr, err)}
	}
	if len(fields.spelling)+1+len(zone.String()) > maxRepeat {
		return Repeat{err: fmt.Errorf("%w: jobs: repeat exceeds %d bytes", tinystore.ErrLimit, maxRepeat)}
	}
	fields.zone = zone
	if _, runs := fields.next(time.Unix(0, 0)); !runs {
		return Repeat{err: fmt.Errorf("%w: jobs: repeat %q never runs", tinystore.ErrInvalid, expr)}
	}
	return Repeat{text: fields.spelling + " " + zone.String(), cron: fields}
}

// Daily repeats a job every day at clock, "15:04", in zone.
func Daily(clock string, zone *time.Location) Repeat {
	hour, minute, found := strings.Cut(clock, ":")
	h, hourErr := strconv.Atoi(hour)
	m, minuteErr := strconv.Atoi(minute)
	if !found || hourErr != nil || minuteErr != nil || len(minute) != 2 || h < 0 || h > 23 || m < 0 || m > 59 {
		return Repeat{err: fmt.Errorf("%w: jobs: Daily(%q): a time of day is 15:04", tinystore.ErrInvalid, clock)}
	}
	return Cron(fmt.Sprintf("%d %d * * *", m, h), zone)
}

// Every repeats a job every d, at the multiples of d since the Unix epoch, so
// that a restart does not shift it: Every(15*time.Minute) runs at :00, :15, :30
// and :45. d is at least a second.
func Every(d time.Duration, options ...EveryOption) Repeat {
	if d < time.Second {
		return Repeat{err: fmt.Errorf("%w: jobs: Every(%v): at least a second", tinystore.ErrInvalid, d)}
	}
	var said everySettings
	for _, option := range options {
		option(&said)
	}
	return Repeat{text: everyText(d, 0), every: d, spread: said.spread}
}

// EveryOption says where in its interval an Every runs.
type EveryOption func(*everySettings)

type everySettings struct{ spread bool }

// Spread runs each key's job at a phase of its own within the interval, the
// key's FNV-1a hash modulo d, so that the jobs of many keys repeating Every(d)
// do not all run in the same instant. The phase is kept with the job, so a
// restart does not shift it either:
//
//	Every(30*time.Second, Spread()), key probe:7   @every 30s +6178ms   at :06.178 and :36.178
func Spread() EveryOption {
	return func(s *everySettings) { s.spread = true }
}

// of is the repeat a job under key keeps: a Spread interval takes the key's
// phase
func (r Repeat) of(key string) Repeat {
	if !r.spread {
		return r
	}
	r.phase = phaseOf(key, r.every.Milliseconds())
	r.text, r.spread = everyText(r.every, r.phase), false
	return r
}

// phaseOf is a key's place within an interval of every milliseconds: its
// FNV-1a hash modulo every
func phaseOf(key string, every int64) time.Duration {
	hash := fnv.New64a()
	hash.Write([]byte(key))
	return time.Duration(hash.Sum64()%uint64(every)) * time.Millisecond //nolint:gosec // every is positive, so is this
}

// everyText is an interval as jobs.db keeps it, with its phase when it has
// one: @every 30s, @every 30s +6178ms
func everyText(every, phase time.Duration) string {
	if phase == 0 {
		return "@every " + spellInterval(every)
	}
	return "@every " + spellInterval(every) + " +" + spellInterval(phase)
}

// next is the first time after after that the repeat runs
func (r Repeat) next(after time.Time) time.Time {
	if r.every > 0 {
		step, phase := r.every.Milliseconds(), r.phase.Milliseconds()
		return time.UnixMilli((floorDiv(after.UnixMilli()-phase, step)+1)*step + phase)
	}
	t, _ := r.cron.next(after)
	return t
}

// floorDiv is a / b rounded down for a positive b, where Go's / rounds toward
// zero
func floorDiv(a, b int64) int64 {
	if a%b < 0 {
		return a/b - 1
	}
	return a / b
}

// parseRepeat reads a repeat as jobs.db keeps it
func parseRepeat(text string) (Repeat, error) {
	if len(text) > maxRepeat {
		return Repeat{}, fmt.Errorf("%w: jobs: a kept repeat exceeds %d bytes", tinystore.ErrCorrupt, maxRepeat)
	}
	if interval, found := strings.CutPrefix(text, "@every "); found {
		return parseEvery(text, interval)
	}
	cut := strings.LastIndexByte(text, ' ')
	if cut < 0 {
		return Repeat{}, fmt.Errorf("%w: jobs: a kept repeat %q has no zone", tinystore.ErrCorrupt, text)
	}
	zone, err := time.LoadLocation(text[cut+1:])
	if err != nil {
		return Repeat{}, fmt.Errorf("jobs: a kept repeat %q: %w", text, err)
	}
	repeat := Cron(text[:cut], zone)
	return repeat, repeat.err
}

// parseEvery reads a kept interval, and its phase when it has one
func parseEvery(text, interval string) (Repeat, error) {
	interval, phaseText, phased := strings.Cut(interval, " +")
	d, err := parseInterval(interval)
	if err != nil {
		return Repeat{}, fmt.Errorf("%w: jobs: a kept repeat %q: %w", tinystore.ErrCorrupt, text, err)
	}
	repeat := Every(d)
	if repeat.err != nil {
		return Repeat{}, fmt.Errorf("%w: jobs: a kept repeat %q: %w", tinystore.ErrCorrupt, text, repeat.err)
	}
	if !phased {
		return repeat, nil
	}
	if repeat.phase, err = parseInterval(phaseText); err != nil || repeat.phase >= d {
		return Repeat{}, fmt.Errorf("%w: jobs: a kept repeat %q has a phase outside its interval",
			tinystore.ErrCorrupt, text)
	}
	repeat.text = everyText(d, repeat.phase)
	return repeat, nil
}

func checkZone(zone *time.Location) error {
	if zone == nil || zone.String() == "Local" || zone.String() == "" {
		return fmt.Errorf("a zone needs a name, such as time.UTC or time.LoadLocation(\"Europe/Moscow\")")
	}
	if _, err := time.LoadLocation(zone.String()); err != nil {
		return fmt.Errorf("zone %q cannot be loaded again by name: %w", zone.String(), err)
	}
	return nil
}

// spellInterval writes an interval in its largest whole unit, as any language
// reads it back: 15m, 90s, 1500ms
func spellInterval(d time.Duration) string {
	for _, unit := range []struct {
		suffix string
		size   time.Duration
	}{{"h", time.Hour}, {"m", time.Minute}, {"s", time.Second}} {
		if d%unit.size == 0 {
			return strconv.FormatInt(int64(d/unit.size), 10) + unit.suffix
		}
	}
	return strconv.FormatInt(d.Milliseconds(), 10) + "ms"
}

func parseInterval(text string) (time.Duration, error) {
	for _, unit := range []struct {
		suffix string
		size   time.Duration
	}{{"ms", time.Millisecond}, {"h", time.Hour}, {"m", time.Minute}, {"s", time.Second}} {
		if number, found := strings.CutSuffix(text, unit.suffix); found {
			n, err := strconv.ParseInt(number, 10, 64)
			if err != nil || n < 1 {
				return 0, fmt.Errorf("an interval of %q", text)
			}
			if n > math.MaxInt64/int64(unit.size) {
				return 0, fmt.Errorf("an interval of %q exceeds time.Duration", text)
			}
			return time.Duration(n) * unit.size, nil
		}
	}
	return 0, fmt.Errorf("an interval of %q", text)
}

// cron is a parsed expression: a bit a value in each field, and whether the
// days of the month and of the week were each left as *
type cron struct {
	spelling           string
	minutes            uint64
	hours              uint64
	days               uint64
	months             uint64
	weekdays           uint64
	anyDay, anyWeekday bool
	zone               *time.Location
}

var cronShorthands = map[string]string{
	"@yearly": "0 0 1 1 *", "@annually": "0 0 1 1 *", "@monthly": "0 0 1 * *", "@weekly": "0 0 * * 0",
	"@daily": "0 0 * * *", "@midnight": "0 0 * * *", "@hourly": "0 * * * *",
}

var (
	monthNames   = []string{"jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"}
	weekdayNames = []string{"sun", "mon", "tue", "wed", "thu", "fri", "sat"}
)

// parseCron reads five fields, or a shorthand for them
func parseCron(expr string) (cron, error) {
	expr = strings.TrimSpace(expr)
	if full, found := cronShorthands[strings.ToLower(expr)]; found {
		expr = full
	}
	fields := strings.Fields(expr)
	if len(fields) != 5 {
		return cron{}, fmt.Errorf("%d fields, not five", len(fields))
	}
	c := cron{spelling: strings.Join(fields, " "), anyDay: fields[2] == "*", anyWeekday: fields[4] == "*"}
	var bitsOf [5]uint64
	for i, field := range []struct {
		low, high int
		names     []string
	}{{0, 59, nil}, {0, 23, nil}, {1, 31, nil}, {1, 12, monthNames}, {0, 7, weekdayNames}} {
		var err error
		if bitsOf[i], err = parseField(fields[i], field.low, field.high, field.names); err != nil {
			return cron{}, err
		}
	}
	weekdays := bitsOf[4]
	if weekdays&(1<<7) != 0 {
		weekdays |= 1 // 7 is Sunday, as 0 is
	}
	c.minutes, c.hours, c.days = bitsOf[0], bitsOf[1], bitsOf[2]
	c.months, c.weekdays = bitsOf[3], weekdays&0x7f
	return c, nil
}

// parseField reads a list of items, each *, a value, or a range, each with an
// optional step: */15, 1-5, 9-18/3, mon-fri
func parseField(field string, low, high int, names []string) (uint64, error) {
	var set uint64
	for item := range strings.SplitSeq(field, ",") {
		span, stepText, stepped := strings.Cut(item, "/")
		step := 1
		if stepped {
			var err error
			if step, err = strconv.Atoi(stepText); err != nil || step < 1 {
				return 0, fmt.Errorf("a step of %q", item)
			}
		}
		first, last, err := parseSpan(span, low, high, names)
		if err != nil {
			return 0, err
		}
		if stepped && first == last && span != "*" {
			last = high
		}
		for value := first; value <= last; {
			set |= 1 << value
			if step > last-value {
				break
			}
			value += step
		}
	}
	return set, nil
}

func parseSpan(span string, low, high int, names []string) (first, last int, err error) {
	if span == "*" {
		return low, high, nil
	}
	from, to, ranged := strings.Cut(span, "-")
	if first, err = parseValue(from, low, high, names); err != nil {
		return 0, 0, err
	}
	last = first
	if ranged {
		if last, err = parseValue(to, low, high, names); err != nil {
			return 0, 0, err
		}
	}
	if last < first {
		return 0, 0, fmt.Errorf("a range of %q", span)
	}
	return first, last, nil
}

func parseValue(text string, low, high int, names []string) (int, error) {
	for i, name := range names {
		if strings.EqualFold(text, name) {
			return i + low, nil
		}
	}
	value, err := strconv.Atoi(text)
	if err != nil || value < low || value > high {
		return 0, fmt.Errorf("a value of %q, not %d to %d", text, low, high)
	}
	return value, nil
}

// the longest a search for the next time looks ahead
const cronYears = 5

// next is the first time after after whose wall clock in the zone the fields
// accept. A wall time daylight saving skips runs when the skip ends; one it
// repeats maps to one instant, so it runs once.
func (c cron) next(after time.Time) (time.Time, bool) {
	wall := after.In(c.zone)
	year, month, day := wall.Date()
	hour, minute := wall.Hour(), wall.Minute()+1
	for ; year <= wall.Year()+cronYears; year, month, day, hour, minute = year+1, 1, 1, 0, 0 {
		for ; month <= 12; month, day, hour, minute = month+1, 1, 0, 0 {
			if c.months&(1<<month) == 0 {
				continue
			}
			for last := daysIn(year, month); day <= last; day, hour, minute = day+1, 0, 0 {
				if !c.dayMatches(year, month, day) {
					continue
				}
				if t, found := c.firstInDay(year, month, day, hour, minute, after); found {
					return t, true
				}
			}
		}
	}
	return time.Time{}, false
}

// firstInDay is the first accepted time of one day from hour and minute on
func (c cron) firstInDay(year int, month time.Month, day, hour, minute int, after time.Time) (time.Time, bool) {
	for ; hour < 24; hour, minute = hour+1, 0 {
		if c.hours&(1<<hour) == 0 {
			continue
		}
		for ; minute < 60; minute++ {
			rest := c.minutes & (^uint64(0) << minute)
			if rest == 0 {
				break
			}
			minute = bits.TrailingZeros64(rest)
			if t := c.instant(year, month, day, hour, minute); t.After(after) {
				return t, true
			}
		}
	}
	return time.Time{}, false
}

// instant is a wall time of the zone as an instant. When daylight saving skips
// it, the result is the instant the skip ends.
//
// time.Date maps a skipped wall time to one before the skip or after it, and
// the skip's edge is where that zone period ends or begins.
func (c cron) instant(year int, month time.Month, day, hour, minute int) time.Time {
	t := time.Date(year, month, day, hour, minute, 0, 0, c.zone)
	if t.Hour() == hour && t.Minute() == minute {
		return t
	}
	start, end := t.ZoneBounds()
	if t.Hour()*60+t.Minute() < hour*60+minute {
		return end
	}
	return start
}

// dayMatches takes a day by its day of the month or of the week, either when
// both are given, as cron always has
func (c cron) dayMatches(year int, month time.Month, day int) bool {
	byDay := c.days&(1<<day) != 0
	byWeekday := c.weekdays&(1<<time.Date(year, month, day, 12, 0, 0, 0, time.UTC).Weekday()) != 0
	switch {
	case c.anyDay && c.anyWeekday:
		return true
	case c.anyDay:
		return byWeekday
	case c.anyWeekday:
		return byDay
	}
	return byDay || byWeekday
}

func daysIn(year int, month time.Month) int {
	return time.Date(year, month+1, 0, 12, 0, 0, 0, time.UTC).Day()
}
