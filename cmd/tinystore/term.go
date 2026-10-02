package main

import (
	"fmt"
	"os"
)

// what a terminal shows: whether a file is one, and whether it shows colours,
// as the store's own console decides it. The tool may require the store's
// module and not its internal packages, so it keeps these lines of its own.

func isTerminal(f *os.File) bool {
	info, err := f.Stat()
	return err == nil && info.Mode()&os.ModeCharDevice != 0
}

// colors says that f is a terminal that shows colours, and turns them on where
// a console needs that: not under NO_COLOR, nor on a dumb terminal
func colors(f *os.File) bool {
	if os.Getenv("NO_COLOR") != "" || os.Getenv("TERM") == "dumb" || !isTerminal(f) {
		return false
	}
	return enableColors(f)
}

// paint writes text in the colours of a terminal that shows them, and plain
// otherwise
type paint bool

const (
	bold   = "1"
	dim    = "2"
	red    = "31"
	green  = "32"
	yellow = "33"
	cyan   = "36"
)

func (p paint) in(style, text string) string {
	if !p {
		return text
	}
	return fmt.Sprintf("\x1b[%sm%s\x1b[0m", style, text)
}
