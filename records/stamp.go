package records

import (
	"math"
	"strings"
)

// a text value keeps each time it spells as its distance behind its record's
// time, in the unit its fraction counts, and the layout that spells it again;
// the text keeps the rest:
//
//	record    2026-09-23 00:47:32.101187376
//	text      I20260923 00:47:32.100929   141 raft_server.h:60] Peer refresh succeeded!
//	stamp     1 byte in, "YMD h:m:s.6", 258 µs behind
//	rest      I   141 raft_server.h:60] Peer refresh succeeded!
//
// a layout is the fields a stamp spells, in order, and the bytes between them
// as they are; a field is a byte below 0x20, a spelled byte never is
const (
	fieldYear      byte = 1 + iota // four digits
	fieldMonth                     // two digits
	fieldMonthName                 // Jan to Dec
	fieldDay                       // two digits
	fieldDaySpaced                 // " 5" or "25", as syslog spells it
	fieldHour
	fieldMinute
	fieldSecond
	fieldFraction // then how many digits it has, one to nine
)

const (
	maxStamps      = 4  // stamps one value keeps
	stampReach     = 64 // bytes a stamp may start after the start, or after the one before it
	maxLayout      = 64 // bytes of one layout
	maxLayouts     = 64 // layouts one column holds
	maxStampDigits = 9  // a fraction's digits: nanoseconds
	secondsInDay   = 86_400
	nanosInDay     = secondsInDay * 1_000_000_000
)

// stampPatterns are the layouts a stamp is looked for in, each perhaps followed
// by a fraction. Y year, M month, N its name, D day, E a day a space pads, and
// h m s the time of day.
var stampPatterns = patterns(
	"Y-M-D h:m:s", // Python, log4j, Postgres
	"Y-M-DTh:m:s", // RFC 3339
	"Y/M/D h:m:s", // Go's log
	"YMD h:m:s",   // glog with its year
	"MD h:m:s",    // glog
	"D N Y h:m:s", // Redis
	"N E h:m:s",   // syslog
	"D/N/Y:h:m:s", // access logs
)

func patterns(spellings ...string) []string {
	fields := strings.NewReplacer("Y", string(fieldYear), "M", string(fieldMonth), "N", string(fieldMonthName),
		"D", string(fieldDay), "E", string(fieldDaySpaced), "h", string(fieldHour), "m", string(fieldMinute),
		"s", string(fieldSecond))
	layouts := make([]string, len(spellings))
	for i, spelling := range spellings {
		layouts[i] = fields.Replace(spelling)
	}
	return layouts
}

var monthNames = [12]string{"Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"}

// stamp is one time a text spells: where, in which pattern and fraction, and
// the time as if it were UTC, counted in the layout's unit
type stamp struct {
	start, end int
	pattern    int
	separator  byte // before the fraction; zero without one
	digits     byte // the fraction's
	wall       int64
}

// key names the stamp's layout without spelling it
func (s *stamp) key() int {
	return s.pattern<<16 | int(s.separator)<<8 | int(s.digits)
}

func (s *stamp) layout() string {
	if s.separator == 0 {
		return stampPatterns[s.pattern]
	}
	return stampPatterns[s.pattern] + string([]byte{s.separator, fieldFraction, s.digits})
}

// findStamps appends the stamps of text, each starting within stampReach
// bytes of the end of the one before; at is its record's time
func findStamps(found []stamp, text string, at int64) []stamp {
	from := 0
	for range maxStamps {
		next, ok := nextStamp(text, from, at)
		if !ok {
			break
		}
		found = append(found, next)
		from = next.end
	}
	return found
}

const (
	shortestStamp = 13         // the fewest bytes a pattern spells: "0923 00:47:32"
	monthInitials = "ADFJMNOS" // the letters a month's name begins with
)

// nextStamp tries the patterns at each place a stamp could start. Those that
// begin with a digit are tried at a digit, and the one that begins with a
// month's name at its initial.
func nextStamp(text string, from int, at int64) (stamp, bool) {
	for i := from; i < min(len(text)-shortestStamp+1, from+stampReach); i++ {
		byName := strings.IndexByte(monthInitials, text[i]) >= 0
		if !byName && !isDigit(text[i]) {
			continue
		}
		for pattern, layout := range stampPatterns {
			if (layout[0] == fieldMonthName) != byName {
				continue
			}
			if found, ok := readStamp(text, i, layout, at); ok {
				found.pattern = pattern
				return found, true
			}
		}
	}
	return stamp{}, false
}

func isDigit(c byte) bool {
	return c >= '0' && c <= '9'
}

// readStamp reads a stamp in one pattern at text[i:], and the fraction after
// it, and keeps it only when it is a time a calendar and a clock can show
func readStamp(text string, i int, layout string, at int64) (stamp, bool) {
	var spelled civil
	end, ok := spelled.read(text, i, layout)
	if !ok {
		return stamp{}, false
	}
	found := stamp{start: i, end: end}
	if end+1 < len(text) && (text[end] == '.' || text[end] == ',') && isDigit(text[end+1]) {
		found.separator = text[end]
		found.end, spelled.fraction, found.digits = readFraction(text, end+1)
	}
	if strings.IndexByte(layout, fieldYear) < 0 {
		spelled.year, _, _ = civilFromDays(floorDiv(at, nanosInDay))
	}
	if !spelled.valid() {
		return stamp{}, false
	}
	found.wall, ok = spelled.wall(int(found.digits))
	return found, ok
}

// civil is a date and a time of day as a stamp spells them
type civil struct {
	year, month, day, hour, minute, second int64
	fraction                               int64
}

// read takes the fields of a layout at text[i:], and says where they end
func (c *civil) read(text string, i int, layout string) (int, bool) {
	for _, field := range []byte(layout) {
		value, next, ok := readField(text, i, field)
		if !ok {
			return 0, false
		}
		switch field {
		case fieldYear:
			c.year = value
		case fieldMonth, fieldMonthName:
			c.month = value
		case fieldDay, fieldDaySpaced:
			c.day = value
		case fieldHour:
			c.hour = value
		case fieldMinute:
			c.minute = value
		case fieldSecond:
			c.second = value
		}
		i = next
	}
	return i, true
}

func readField(text string, i int, field byte) (value int64, next int, ok bool) {
	switch field {
	case fieldYear:
		return readDigits(text, i, 4)
	case fieldMonthName:
		for month, name := range monthNames {
			if strings.HasPrefix(text[i:], name) {
				return int64(month) + 1, i + len(name), true
			}
		}
		return 0, i, false
	case fieldDaySpaced:
		if i+1 < len(text) && text[i] == ' ' && text[i+1] >= '1' && text[i+1] <= '9' {
			return int64(text[i+1] - '0'), i + 2, true
		}
		if i < len(text) && text[i] == '0' {
			return 0, i, false
		}
		return readDigits(text, i, 2)
	case fieldMonth, fieldDay, fieldHour, fieldMinute, fieldSecond:
		return readDigits(text, i, 2)
	default: // a byte between fields, spelled as it is
		return 0, i + 1, i < len(text) && text[i] == field
	}
}

func readDigits(text string, i, width int) (value int64, next int, ok bool) {
	if i+width > len(text) {
		return 0, i, false
	}
	for _, digit := range []byte(text[i : i+width]) {
		if !isDigit(digit) {
			return 0, i, false
		}
		value = value*10 + int64(digit-'0')
	}
	return value, i + width, true
}

// readFraction takes up to nine digits; any more stay in the text
func readFraction(text string, i int) (end int, fraction int64, digits byte) {
	for end = i; end < len(text) && isDigit(text[end]) && digits < maxStampDigits; end++ {
		fraction = fraction*10 + int64(text[end]-'0')
		digits++
	}
	return end, fraction, digits
}

// valid is a day the calendar has, at a time of day a clock shows; a leap
// second is not one, and stays text
func (c *civil) valid() bool {
	if c.month < 1 || c.month > 12 || c.day < 1 || c.hour > 23 || c.minute > 59 || c.second > 59 {
		return false
	}
	year, month, day := civilFromDays(daysFromCivil(c.year, c.month, c.day))
	return year == c.year && month == c.month && day == c.day
}

// wall is the time as if it were UTC, in units of a fraction of digits digits;
// a time past what an int64 of those units holds is not a stamp
func (c *civil) wall(digits int) (int64, bool) {
	seconds := daysFromCivil(c.year, c.month, c.day)*secondsInDay + c.hour*3600 + c.minute*60 + c.second
	scale := pow10(digits)
	if seconds > (math.MaxInt64-c.fraction)/scale || seconds < math.MinInt64/scale {
		return 0, false
	}
	return seconds*scale + c.fraction, true
}

// the proleptic Gregorian calendar as days since 1970-01-01, and back:
//
//	1970-01-01 → 0      2000-03-01 → 11017      1969-12-31 → -1
func daysFromCivil(year, month, day int64) int64 {
	if month <= 2 {
		year--
	}
	era := floorDiv(year, 400)
	yearOfEra := year - era*400
	dayOfYear := (153*((month+9)%12)+2)/5 + day - 1
	dayOfEra := yearOfEra*365 + yearOfEra/4 - yearOfEra/100 + dayOfYear
	return era*146_097 + dayOfEra - 719_468
}

func civilFromDays(days int64) (year, month, day int64) {
	days += 719_468
	era := floorDiv(days, 146_097)
	dayOfEra := days - era*146_097
	yearOfEra := (dayOfEra - dayOfEra/1460 + dayOfEra/36_524 - dayOfEra/146_096) / 365
	dayOfYear := dayOfEra - (365*yearOfEra + yearOfEra/4 - yearOfEra/100)
	shifted := (5*dayOfYear + 2) / 153
	day = dayOfYear - (153*shifted+2)/5 + 1
	month = shifted + 3
	if month > 12 {
		month -= 12
	}
	year = yearOfEra + era*400
	if month <= 2 {
		year++
	}
	return year, month, day
}

func floorDiv(a, b int64) int64 {
	quotient := a / b
	if a%b != 0 && (a < 0) != (b < 0) {
		quotient--
	}
	return quotient
}

func pow10(digits int) int64 {
	scale := int64(1)
	for range digits {
		scale *= 10
	}
	return scale
}

// distanceBehind is how far a time counted in units of 10^unit nanoseconds lies
// behind a record's time at, and valueBehind the time that lies distance behind
// it; false when an int64 cannot hold the result:
//
//	at 1727300000123456789 ns, value 1727300000121 in milliseconds (unit 6) → 2 behind
func distanceBehind(at, value int64, unit int) (int64, bool) {
	return subtract(floorDiv(at, pow10(unit)), value)
}

func valueBehind(at, distance int64, unit int) (int64, bool) {
	return subtract(floorDiv(at, pow10(unit)), distance)
}

// subtract is a - b, and false when an int64 cannot hold it
func subtract(a, b int64) (int64, bool) {
	difference := a - b
	return difference, (a >= 0) == (b >= 0) || (difference >= 0) == (a >= 0)
}

// stampedColumn is a column's values cut apart: the rest of each, and its stamps
type stampedColumn struct {
	layouts   []string
	known     map[int]int // a layout's key to its index in layouts
	counts    []int64     // how many stamps each value holds
	layoutIDs []int64     // each stamp's index in layouts
	gaps      []int64     // bytes of the rest between a stamp and the one before it in its value
	behind    []int64     // how far a stamp lies behind its record's time, in its unit
	rest      []string
}

// findColumnStamps finds every value's stamps, and says whether they are
// enough to pay for the columns they take: at least eight, one a value in eight
func (e *encoder) findColumnStamps(values []string, times []int64) bool {
	e.stamps, e.stampEnds = e.stamps[:0], e.stampEnds[:0]
	for i, value := range values {
		e.stamps = findStamps(e.stamps, value, times[i])
		e.stampEnds = append(e.stampEnds, len(e.stamps))
	}
	return len(e.stamps) >= 8 && len(e.stamps)*8 >= len(values)
}

// cutStamps takes each value's stamps, found already, out of it. A stamp whose
// layout the column has no room for, or whose distance an int64 cannot hold,
// stays text.
func (e *encoder) cutStamps(values []string, times []int64) *stampedColumn {
	column := &stampedColumn{known: map[int]int{}}
	var rest joiner
	first := 0
	for i, value := range values {
		from, kept := 0, int64(0)
		for _, found := range e.stamps[first:e.stampEnds[i]] {
			layoutID, fits := column.layoutOf(&found)
			distance, near := distanceBehind(times[i], found.wall, maxStampDigits-int(found.digits))
			if !fits || !near {
				break
			}
			rest.text.WriteString(value[from:found.start])
			column.layoutIDs = append(column.layoutIDs, int64(layoutID))
			column.gaps = append(column.gaps, int64(found.start-from))
			column.behind = append(column.behind, distance)
			from = found.end
			kept++
		}
		rest.text.WriteString(value[from:])
		rest.end()
		column.counts = append(column.counts, kept)
		first = e.stampEnds[i]
	}
	column.rest = rest.values()
	return column
}

func (s *stampedColumn) layoutOf(found *stamp) (int, bool) {
	if id, ok := s.known[found.key()]; ok {
		return id, true
	}
	if len(s.layouts) == maxLayouts {
		return 0, false
	}
	s.known[found.key()] = len(s.layouts)
	s.layouts = append(s.layouts, found.layout())
	return len(s.layouts) - 1, true
}

// appendStamped writes values whose stamps pay for the columns they take
func (e *encoder) appendStamped(out []byte, flags byte, times []int64) ([]byte, bool) {
	if times == nil || !e.findColumnStamps(e.inner, times) {
		return out, false
	}
	column := e.cutStamps(e.inner, times)
	out = appendStrings(append(out, flags|valueStamped), column.layouts)
	for _, ints := range [][]int64{column.counts, column.layoutIDs, column.gaps, column.behind} {
		out = e.appendInts(out, ints)
	}
	return e.appendTexts(out, column.rest), true
}

// stampedValues rebuilds a column's values from the rest of each and its
// stamps, and each value's record time
func (d *decoder) stampedValues(c *cursor, count int, times rowTimes) []string {
	column := stampedColumn{layouts: readLayouts(c), counts: d.ints(c, count)}
	total := stampTotal(c, column.counts)
	column.layoutIDs, column.gaps, column.behind = d.ints(c, total), d.ints(c, total), d.ints(c, total)
	column.rest = d.texts(c, count)
	at := timesOf(c, times, count)
	if c.err != nil {
		return nil
	}
	return column.rebuild(c, d, at)
}

func readLayouts(c *cursor) []string {
	layouts := c.strings(maxLayouts, maxLayout)
	for _, layout := range layouts {
		if _, ok := layoutDigits(layout); !ok {
			c.fail("stamp layout")
		}
	}
	return layouts
}

// layoutDigits checks a layout and returns its fraction's digits: fields
// below 0x20, the fraction once, with its count, and nothing else below 0x20
func layoutDigits(layout string) (digits int, ok bool) {
	fractions := 0
	for i := 0; i < len(layout); i++ {
		switch field := layout[i]; {
		case field == fieldFraction && i+1 < len(layout) && layout[i+1] >= 1 && layout[i+1] <= maxStampDigits:
			digits, fractions = int(layout[i+1]), fractions+1
			i++
		case field < fieldYear || (field > fieldSecond && field < 0x20):
			return 0, false
		}
	}
	return digits, len(layout) > 0 && fractions <= 1
}

func stampTotal(c *cursor, counts []int64) int {
	total := 0
	for _, count := range counts {
		if count < 0 || count > maxStamps {
			c.fail("stamps of one value")
			return 0
		}
		total += int(count)
	}
	return total
}

// rebuild charges what the values will hold before it builds them, then
// puts each stamp back where it was taken from
func (s *stampedColumn) rebuild(c *cursor, d *decoder, times []int64) []string {
	size, ok := s.rebuiltSize()
	if !ok {
		c.fail("stamp layout reference")
		return nil
	}
	if !c.expand(size) {
		return nil
	}
	rebuilt := stampRebuild{column: s, spelled: d.spelled}
	rebuilt.values.text.Grow(size)
	for i := range s.rest {
		if !rebuilt.add(i, times[i]) {
			c.fail("stamp place or time")
			return nil
		}
	}
	d.spelled = rebuilt.spelled
	return rebuilt.values.values()
}

// rebuiltSize is how many bytes the values hold once their stamps are back
func (s *stampedColumn) rebuiltSize() (int, bool) {
	size := 0
	for _, rest := range s.rest {
		size += len(rest)
	}
	for _, id := range s.layoutIDs {
		if id < 0 || id >= int64(len(s.layouts)) {
			return 0, false
		}
		size += layoutWidth(s.layouts[id])
	}
	return size, true
}

// stampRebuild puts a column's values back together one after another
type stampRebuild struct {
	column  *stampedColumn
	values  joiner
	next    int    // the first stamp of the value added next
	spelled []byte // one stamp as its layout spells it
}

// add writes value i with its stamps, spelled from its record's time at
func (r *stampRebuild) add(i int, at int64) bool {
	s := r.column
	rest, from := s.rest[i], 0
	for range s.counts[i] {
		gap := s.gaps[r.next]
		if gap < 0 || gap > int64(len(rest)-from) {
			return false
		}
		r.values.text.WriteString(rest[from : from+int(gap)])
		from += int(gap)
		if !r.spell(s.layouts[s.layoutIDs[r.next]], at, s.behind[r.next]) {
			return false
		}
		r.values.text.Write(r.spelled)
		r.next++
	}
	r.values.text.WriteString(rest[from:])
	r.values.end()
	return true
}

// spell writes into spelled the time behind lies behind at, as layout spells it
func (r *stampRebuild) spell(layout string, at, behind int64) bool {
	digits, _ := layoutDigits(layout)
	wall, ok := valueBehind(at, behind, maxStampDigits-digits)
	if !ok {
		return false
	}
	r.spelled, ok = appendStamp(r.spelled[:0], layout, wall, digits)
	return ok
}

func layoutWidth(layout string) int {
	width := 0
	for i := 0; i < len(layout); i++ {
		switch layout[i] {
		case fieldYear:
			width += 4
		case fieldMonthName:
			width += 3
		case fieldMonth, fieldDay, fieldDaySpaced, fieldHour, fieldMinute, fieldSecond:
			width += 2
		case fieldFraction:
			i++
			width += int(layout[i])
		default:
			width++
		}
	}
	return width
}

// appendStamp spells a time counted in units of a fraction of digits digits
// as a layout does; false for a year four digits cannot spell
func appendStamp(out []byte, layout string, wall int64, digits int) ([]byte, bool) {
	seconds := floorDiv(wall, pow10(digits))
	fraction := wall - seconds*pow10(digits)
	days := floorDiv(seconds, secondsInDay)
	ofDay := seconds - days*secondsInDay
	year, month, day := civilFromDays(days)
	for i := 0; i < len(layout); i++ {
		switch field := layout[i]; field {
		case fieldYear:
			if year < 0 || year > 9999 {
				return out, false
			}
			out = appendPadded(out, year, 4)
		case fieldMonth:
			out = appendPadded(out, month, 2)
		case fieldMonthName:
			out = append(out, monthNames[month-1]...)
		case fieldDay:
			out = appendPadded(out, day, 2)
		case fieldDaySpaced:
			out = appendSpaced(out, day)
		case fieldHour:
			out = appendPadded(out, ofDay/3600, 2)
		case fieldMinute:
			out = appendPadded(out, ofDay/60%60, 2)
		case fieldSecond:
			out = appendPadded(out, ofDay%60, 2)
		case fieldFraction:
			i++
			out = appendPadded(out, fraction, digits)
		default:
			out = append(out, field)
		}
	}
	return out, true
}

// appendPadded spells a value of at most width digits, zeros first
func appendPadded(out []byte, value int64, width int) []byte {
	for scale := pow10(width - 1); scale > 0; scale /= 10 {
		out = append(out, byte('0'+value/scale%10))
	}
	return out
}

func appendSpaced(out []byte, day int64) []byte {
	if day < 10 {
		return appendPadded(append(out, ' '), day, 1)
	}
	return appendPadded(out, day, 2)
}
