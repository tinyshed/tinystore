package logline

import (
	"io"
	"log/slog"
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

// Read is the settings the options give, the environment's variables over
// them, and what of the environment could not be read.
func Read(options []Option) (Settings, []Unread) {
	s := Settings{Env: DefaultEnvironment}
	for _, option := range options {
		option.Apply(&s)
	}
	return s.Env.over(s)
}
