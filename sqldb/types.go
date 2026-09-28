package sqldb

import (
	"bytes"
	"encoding/json"
	"fmt"
	"time"
)

// JSON holds V, which its column keeps as JSON text through encoding/json. A
// JSON holding a nil slice or map writes null, a valid JSON text; the column is
// NULL only through a pointer to the JSON.
type JSON[T any] struct{ V T }

// JSONOf wraps v, as a query's argument or a field's value.
func JSONOf[T any](v T) JSON[T] {
	return JSON[T]{V: v}
}

// jsonText is what a JSON writes, HTML left unescaped so that a query sees
// the text it was given
func (j JSON[T]) jsonText() (string, error) {
	var out bytes.Buffer
	encoder := json.NewEncoder(&out)
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(j.V); err != nil {
		return "", err
	}
	return string(bytes.TrimSuffix(out.Bytes(), []byte("\n"))), nil
}

func (j *JSON[T]) readJSON(text []byte) error {
	return json.Unmarshal(text, &j.V)
}

// the methods that mark a JSON of any T, which reflection cannot name
type (
	jsonValue  interface{ jsonText() (string, error) }
	jsonTarget interface{ readJSON(text []byte) error }
)

// Date is a day of the calendar without a time or a zone, kept as its
// YYYY-MM-DD text, which SQLite's date functions read and write.
type Date struct {
	Year  int
	Month time.Month
	Day   int
}

// DateOf is the day t falls on in its own location.
func DateOf(t time.Time) Date {
	year, month, day := t.Date()
	return Date{Year: year, Month: month, Day: day}
}

func (d Date) String() string {
	return fmt.Sprintf("%04d-%02d-%02d", d.Year, int(d.Month), d.Day)
}

// In is the day's first moment in loc.
func (d Date) In(loc *time.Location) time.Time {
	return time.Date(d.Year, d.Month, d.Day, 0, 0, 0, 0, loc)
}

// valid is a day SQLite's date() spells back as it was written:
//
//	2026-02-28 → 2026-02-28    2026-02-30 → 2026-03-02, refused
func (d Date) valid() bool {
	if d.Year < 0 || d.Year > 9999 {
		return false
	}
	year, month, day := d.In(time.UTC).Date()
	return year == d.Year && month == d.Month && day == d.Day
}

func parseDate(text string) (Date, error) {
	day, err := time.Parse(time.DateOnly, text)
	if err != nil {
		return Date{}, err
	}
	return DateOf(day), nil
}
