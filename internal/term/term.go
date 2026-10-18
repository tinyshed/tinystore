// Package term says whether a file is a terminal a person reads, and whether
// it shows colours, for the console lines a logger writes beside the store.
package term

import "os"

// IsTerminal says whether f is a character device, a console or a terminal,
// rather than a pipe or a file. The null device is one too, and nobody reads
// what goes there.
func IsTerminal(f *os.File) bool {
	info, err := f.Stat()
	return err == nil && info.Mode()&os.ModeCharDevice != 0
}

// Colors says whether f shows ANSI colours: a terminal, without NO_COLOR
// (no-color.org) or a TERM of dumb. On Windows it asks the console to read
// escape sequences, which a console from before Windows Terminal prints as
// text unless asked.
//
// FORCE_COLOR other than 0 turns them on where f is a pipe whose reader shows
// them, as an IDE's run console does; NO_COLOR and a dumb TERM still win.
//
//	NO_COLOR=1          off
//	TERM=dumb           off
//	FORCE_COLOR=1       on, a pipe or a file too
//	a terminal          on
//	anything else       off
func Colors(f *os.File) bool {
	switch {
	case os.Getenv("NO_COLOR") != "" || os.Getenv("TERM") == "dumb":
		return false
	case Forced():
		enableColors(f)
		return true
	case !IsTerminal(f):
		return false
	}
	return enableColors(f)
}

// Forced says whether FORCE_COLOR asks for colours: set, and not 0.
func Forced() bool {
	force := os.Getenv("FORCE_COLOR")
	return force != "" && force != "0"
}
