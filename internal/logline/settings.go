package logline

import (
	"io"
	"log/slog"
	"os"
	"time"
)

// Settings is what a handler is told: package console's options, and a store
// handler's own from package records.
type Settings struct {
	Format           Format
	Time             Time
	HideStream       bool
	To               io.Writer // standard error when nil
	Level            slog.Leveler
	Redact           []string
	KeepURLPasswords bool
	Replace          func(groups []string, attr slog.Attr) slog.Attr
	Env              Environment
	AddSource        bool
	Zone             *time.Location // a test's, in place of the machine's own
	// Keep is what a store handler's stream keeps; nil changes nothing.
	Keep *time.Duration
	// Buffer is the lines a store's handler holds for the store; 0 is 1024.
	Buffer int
}

// Option is an option of a handler. Its Apply takes a type of this module's
// own, so only console's and records' options are options.
type Option interface{ Apply(*Settings) }

// Told is what a handler was told: the options' settings with the
// environment's variables over them, the code's alone, and what the
// environment gave and could not be read.
type Told struct {
	Settings
	Code Settings
	// Spelled is each variable the environment gave, by its name, as LOG_ spells it.
	Spelled map[string]string
	Ignored []Unread
}

// Read is what the options and the environment tell a handler.
func Read(options []Option) Told {
	code := Settings{Env: DefaultEnvironment}
	for _, option := range options {
		option.Apply(&code)
	}
	told := Told{Code: code}
	told.Settings, told.Spelled, told.Ignored = code.Env.over(code)
	return told
}

// Shown is how a handler of s prints its lines, as a logger's config names
// it: pretty, json or off, what its console chose when s says none.
func Shown(s Settings) string {
	out := s.To
	if out == nil {
		out = os.Stderr
	}
	switch echo := NewEcho(out, s.Format); {
	case echo == nil:
		return "off"
	case echo.Pretty:
		return "pretty"
	}
	return "json"
}

// TimeName is how a pretty line shows its time, as a logger's config names it.
func TimeName(t Time) string {
	switch t {
	case Full:
		return "full"
	case NoTime:
		return "off"
	}
	return "clock"
}
