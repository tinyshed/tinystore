package records

import (
	"errors"
	"fmt"
	"math"
	"slices"
	"strconv"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// the example in the comment on stamps
func TestAStampIsKeptBehindItsRecordsTime(t *testing.T) {
	at := time.Date(2026, 9, 23, 0, 47, 32, 101187376, time.UTC).UnixNano()
	text := "I20260923 00:47:32.100929   141 raft_server.h:60] Peer refresh succeeded!"
	found := findStamps(nil, text, at)
	if len(found) != 1 || found[0].start != 1 || found[0].layout() != patterns("YMD h:m:s")[0]+".\x09\x06" {
		t.Fatalf("found %+v", found)
	}
	if distance, ok := distanceBehind(at, found[0].wall, maxStampDigits-int(found[0].digits)); !ok || distance != 258 {
		t.Errorf("%d µs behind, want 258", distance)
	}
	if rest := text[:found[0].start] + text[found[0].end:]; rest != "I   141 raft_server.h:60] Peer refresh succeeded!" {
		t.Errorf("rest %q", rest)
	}
}

// every layout a stamp is looked for in is found, spelled back byte for
// byte, and keeps the text around it
func TestEveryStampLayoutSpellsItsTextBack(t *testing.T) {
	at := time.Date(2026, 9, 5, 7, 8, 9, 123456789, time.UTC)
	for _, text := range []string{
		at.Format("2006-01-02 15:04:05,000") + " - app - INFO - ok",
		at.Format("2006-01-02T15:04:05.000000000Z07:00") + " level=info",
		at.Format("2006/01/02 15:04:05") + " started",
		"W" + at.Format("20060102 15:04:05.000000") + "  7 a.cc:1] x",
		"E" + at.Format("0102 15:04:05.000000") + "  7 a.cc:1] x",
		"1:M " + at.Format("02 Jan 2006 15:04:05.000") + " * saved",
		at.Format("Jan _2 15:04:05") + " host sshd[1]: ok",
		at.Add(20*24*time.Hour).Format("Jan _2 15:04:05") + " host sshd[1]: two digits",
		`10.0.0.7 - - [` + at.Format("02/Jan/2006:15:04:05 -0700") + `] "GET / HTTP/1.1" 200`,
		"[" + at.Format("2006-01-02 15:04:05") + "] local.INFO: in brackets",
	} {
		found := findStamps(nil, text, at.UnixNano())
		if len(found) != 1 {
			t.Errorf("%q: %d stamps", text, len(found))
			continue
		}
		spelled, ok := appendStamp(nil, found[0].layout(), found[0].wall, int(found[0].digits))
		if !ok || string(spelled) != text[found[0].start:found[0].end] {
			t.Errorf("%q spelled back as %q", text[found[0].start:found[0].end], spelled)
		}
	}
}

// what no calendar or clock shows stays text, and so does a day syslog would
// have padded with a space
func TestATimeNoClockShowsStaysText(t *testing.T) {
	at := time.Date(2026, 9, 5, 7, 8, 9, 0, time.UTC).UnixNano()
	for _, text := range []string{
		"2026-02-30 12:00:00 no such day",
		"2026-09-23 24:00:00 no such hour",
		"2026-09-23 23:59:60 a leap second",
		"2026-13-01 00:00:00 no such month",
		"Sep 05 12:00:00 a day padded with a zero",
		"Sept 5 12:00:00 a name spelled longer",
		"20260923 00:47 no seconds",
	} {
		if found := findStamps(nil, text, at); len(found) != 0 {
			t.Errorf("%q: found %+v", text, found)
		}
	}
}

// the examples in the comment on daysFromCivil, and the calendar Go keeps
func TestTheCalendarCountsDaysAsGoDoes(t *testing.T) {
	for date, days := range map[[3]int64]int64{{1970, 1, 1}: 0, {2000, 3, 1}: 11017, {1969, 12, 31}: -1} {
		if got := daysFromCivil(date[0], date[1], date[2]); got != days {
			t.Errorf("%v is day %d, want %d", date, got, days)
		}
	}
	for days := int64(-800_000); days < 3_000_000; days += 997 {
		year, month, day := civilFromDays(days)
		want := time.Unix(days*secondsInDay, 0).UTC()
		if year != int64(want.Year()) || month != int64(want.Month()) || day != int64(want.Day()) {
			t.Fatalf("day %d is %d-%d-%d, want %s", days, year, month, day, want)
		}
		if daysFromCivil(year, month, day) != days {
			t.Fatalf("day %d does not come back", days)
		}
	}
}

func roundTripTimed(t *testing.T, values []string, times []int64) byte {
	t.Helper()
	e, d := testCoders(t)
	encoded := e.appendValues(nil, values, times)
	c := cursor{data: encoded, budget: &expansion{limit: maxExpandedText}}
	decoded := d.values(&c, len(values), func() []int64 { return times })
	if err := c.finish(); err != nil {
		t.Fatalf("%q: %v", values, err)
	}
	if !slices.Equal(decoded, values) {
		t.Fatalf("decoded %q, want %q", decoded, values)
	}
	return encoded[0]
}

// a column of text that spells its records' times keeps them apart, and one
// that spells too few stays text
func TestAColumnKeepsItsStampsWhenTheyPay(t *testing.T) {
	var stamped, bodies []string
	var times []int64
	for _, record := range textRecords(400) {
		if record.Body != nil {
			bodies, times = append(bodies, *record.Body), append(times, record.At.UnixNano())
			stamped = append(stamped, strconv.Quote(*record.Body))
		}
	}
	if kind := roundTripTimed(t, bodies, times); kind != valueStamped {
		t.Errorf("bodies typed as %#x", kind)
	}
	if kind := roundTripTimed(t, stamped, times); kind != valueQuoted|valueStamped {
		t.Errorf("quoted bodies typed as %#x", kind)
	}
	few := slices.Repeat([]string{"no time here"}, 60)
	for _, at := range times[:7] {
		few = append(few, time.Unix(0, at).UTC().Format("2006-01-02 15:04:05,000")+" one stamp")
	}
	if kind := roundTripTimed(t, few, times[:len(few)]); kind != valueText {
		t.Errorf("seven stamps in sixty-seven values typed as %#x", kind)
	}
}

// Integers that count their records' time are kept as their distance behind it,
// and integers that do not are kept as they are. The first is the example in
// the comments on valueTimed and distanceBehind.
func TestIntegersThatCountTimeAreKeptBehindIt(t *testing.T) {
	if distance, ok := distanceBehind(1727300000123456789, 1727300000121, 6); !ok || distance != 2 {
		t.Errorf("%d ms behind, want 2", distance)
	}
	if value, ok := valueBehind(1727300000123456789, 2, 6); !ok || value != 1727300000121 {
		t.Errorf("2 ms behind is %d, want 1727300000121", value)
	}
	var clocks, measures []string
	var times []int64
	for i := range 200 {
		at := fixtureBase + int64(i)*1_234_567_891
		times = append(times, at)
		clocks = append(clocks, strconv.FormatInt(at/1_000_000-int64(i%3), 10))
		measures = append(measures, strconv.Itoa(i*7919%1920))
	}
	if kind := roundTripTimed(t, clocks, times); kind != valueInteger|valueTimed {
		t.Errorf("milliseconds typed as %#x", kind)
	}
	if kind := roundTripTimed(t, measures, times); kind != valueInteger {
		t.Errorf("measures typed as %#x", kind)
	}
	extremes := slices.Repeat([]int64{math.MinInt64, math.MaxInt64}, 100)
	if kind := roundTripTimed(t, clocks, extremes); kind != valueInteger {
		t.Errorf("distances past 64 bits typed as %#x", kind)
	}
}

// a record at either end of time keeps its text exactly, stamps or no stamps
func TestStampsAtTheEndsOfTimeStayExact(t *testing.T) {
	var values []string
	for range 16 {
		values = append(values, "9999-12-31 23:59:59.999999999 last", "0000-01-01 00:00:00 first", "1970-01-01 00:00:00.5 z")
	}
	for _, at := range []int64{math.MinInt64, -1, 0, math.MaxInt64} {
		roundTripTimed(t, values, slices.Repeat([]int64{at}, len(values)))
	}
}

// a stamped column read without its records' times is refused, as a context
// value would be read
func TestStampsWithoutTheirTimesAreRefused(t *testing.T) {
	e, d := testCoders(t)
	values, times := make([]string, 16), make([]int64, 16)
	for i := range values {
		times[i] = fixtureBase + int64(i)
		values[i] = time.Unix(0, times[i]).UTC().Format("2006-01-02 15:04:05.000000000") + " line " + fmt.Sprint(i)
	}
	encoded := e.appendValues(nil, values, times)
	c := cursor{data: encoded, budget: &expansion{limit: maxExpandedText}}
	d.values(&c, len(values), nil)
	if err := c.finish(); !errors.Is(err, tinystore.ErrCorrupt) {
		t.Fatalf("stamps without times: %v", err)
	}
}

// a stamp found anywhere is spelled back exactly, whatever the text around it
func FuzzStamps(f *testing.F) {
	for _, record := range textRecords(64) {
		if record.Body != nil {
			f.Add(*record.Body, record.At.UnixNano())
		}
	}
	f.Add("Feb 29 12:00:00 in a year without one", int64(0))
	f.Fuzz(func(t *testing.T, text string, at int64) {
		for _, found := range findStamps(nil, text, at) {
			spelled, ok := appendStamp(nil, found.layout(), found.wall, int(found.digits))
			if !ok || string(spelled) != text[found.start:found.end] {
				t.Fatalf("%q spelled back as %q", text[found.start:found.end], spelled)
			}
		}
		values, times := slices.Repeat([]string{text}, 9), make([]int64, 9)
		for i := range times {
			times[i] = at + int64(i)
		}
		roundTripTimed(t, values, times)
	})
}
