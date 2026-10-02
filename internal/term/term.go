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
func Colors(f *os.File) bool {
	if os.Getenv("NO_COLOR") != "" || os.Getenv("TERM") == "dumb" || !IsTerminal(f) {
		return false
	}
	return enableColors(f)
}
