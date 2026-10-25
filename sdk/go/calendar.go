package tinystore

// Calendar is a span of a calendar an aggregate's bucket covers in its time
// zone: a local day, a week from its Monday, or a month. A metrics or a
// records aggregate takes it with the zone's IANA name, in place of Every.
type Calendar string

const (
	// Day begins at a local midnight, 23 or 25 hours long on the days the clock moves.
	Day Calendar = "day"
	// Week begins at a Monday's local midnight, as ISO 8601 counts weeks.
	Week Calendar = "week"
	// Month begins at the local midnight of a month's first day.
	Month Calendar = "month"
)
