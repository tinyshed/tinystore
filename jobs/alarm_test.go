package jobs

import (
	"testing"
)

func TestAWriteDuringAnAlarmReadCannotBeLost(t *testing.T) {
	a := newAlarm()
	read := a.rung(0)
	if read == nil {
		t.Fatal("the first Work loop did not read the file")
	}
	a.lower(10_000)
	a.set(read, 0) // the query did not see that committed write
	if a.at != 10_000 {
		t.Fatalf("the write was lost behind a stale query: %d", a.at)
	}
	if read = a.rung(10_000); read == nil {
		t.Fatal("the lowered alarm did not ring")
	}
	a.set(read, 20_000)
	if a.at != 20_000 {
		t.Fatalf("the next query could not set the actual due time: %d", a.at)
	}
}

func TestALaterWriteDuringAnAlarmReadLetsTheLoopSleep(t *testing.T) {
	a := newAlarm()
	read := a.rung(1_000)
	a.lower(1_000 + 3_600_000)
	a.set(read, 1_000+60_000)
	if _, sleep := a.wait(1_000); sleep != longestAlarm || a.at != 1_000+60_000 {
		t.Fatalf("a write an hour ahead left the alarm at %d, sleeping %v", a.at, sleep)
	}
}

func TestTwoReadsKeepWhatEachMayHaveMissed(t *testing.T) {
	a := newAlarm()
	first, second := a.rung(0), a.rung(0)
	a.lower(5_000)
	a.set(second, 5_000)
	a.set(first, 9_000) // began before the write, finished after the other read
	if a.at != 5_000 || len(a.reads) != 0 {
		t.Fatalf("two reads left the alarm at %d with %d open", a.at, len(a.reads))
	}
}

func TestAReadWithoutAnAnswerKeepsTheAlarmRinging(t *testing.T) {
	a := newAlarm()
	read := a.rung(0)
	a.forget(read)
	if !a.due(0) || len(a.reads) != 0 {
		t.Fatalf("a forgotten read left the alarm at %d with %d open", a.at, len(a.reads))
	}
}
