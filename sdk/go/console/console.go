// Package console is TinyStore's logger without the store: a log/slog handler
// that writes each line as it is logged, pretty for a person at a terminal and
// one JSON object a line for a collector, the bytes the Bun, Python and Rust
// loggers write. It links no store. A store's handler, records.NewHandler,
// writes the same lines, takes the same options, and keeps every line too.
//
//	log := slog.New(console.NewHandler("app", console.Redact(console.Secrets...)))
//
//	11:02:11.123 WARN  app  slow request  ms=1200 password=[redacted]
package console

import (
	"io"
	"log/slog"
	"os"

	"tinyshed.org/tinystore/internal/logline"
)

// NewHandler writes a logger's lines to standard error, and keeps none.
func NewHandler(stream string, options ...Option) slog.Handler {
	told := logline.Read(options)
	return logline.NewHandler(stream, nil, told.Settings, told.Ignored)
}

// Option changes a logger's lines: on the console, and in the store when the
// handler is a store's.
type Option = logline.Option

// Format is how the console writes a line. Without one, lines are pretty on a
// terminal and JSON otherwise.
type Format int

const (
	Pretty Format = iota + 1 // 11:02:11.123 INFO  api  started  port=3000
	JSON                     // one object a line, for a collector
	Off                      // nothing: a store's handler still keeps each line
)

func (f Format) Apply(s *logline.Settings) { s.Format = logline.Format(f) }

// Time is how a pretty line shows its time; a JSON line always has it.
type Time int

const (
	TimeClock Time = iota + 1 // 11:02:11.123, the default
	TimeFull                  // 2026-10-02 11:02:11.123 +03:00
	TimeOff                   // none, where docker or journald stamp each line
)

func (t Time) Apply(s *logline.Settings) {
	switch t {
	case TimeFull:
		s.Time = logline.Full
	case TimeOff:
		s.Time = logline.NoTime
	default:
		s.Time = logline.Clock
	}
}

// HideStream leaves the stream out of a pretty line; a JSON line and the store keep it.
const HideStream = hiddenStream(true)

type hiddenStream bool

func (h hiddenStream) Apply(s *logline.Settings) { s.HideStream = bool(h) }

// To writes the lines to w instead of standard error.
func To(w io.Writer) Option {
	return optionFunc(func(s *logline.Settings) { s.To = w })
}

// Level keeps lines from level up; without it every line is kept.
func Level(level slog.Leveler) Option {
	return optionFunc(func(s *logline.Settings) { s.Level = level })
}

// Redact hides the values of fields whose keys name a secret, at any depth of
// a JSON value. A key names one when some of its words in a row, written
// together, are a name's: the words of DB_PASSWORD, PasswordHash, api-key and
// APIKey are split at '_', '-', '.', spaces and capitals, the case ignored.
// The message is not searched.
//
//	Redact("password", "api key")  hides  password, DB_PASSWORD, PasswordHash, api_key, apiKey, APIKEY
//	Redact("token")                hides  bot_token, token_count, and not tokens_used
func Redact(names ...string) Option {
	return optionFunc(func(s *logline.Settings) { s.Redact = append(s.Redact, names...) })
}

// Secrets is what Redact takes to hide the usual secrets. It hides a counter
// named like one too, token_count among them.
var Secrets = logline.Secrets

// KeepURLPasswords leaves the password of a URL inside a value as it is. Without
// it, postgres://ann:hunter2@db/app is kept as postgres://ann:[redacted]@db/app.
const KeepURLPasswords = keptURLPasswords(true)

type keptURLPasswords bool

func (k keptURLPasswords) Apply(s *logline.Settings) { s.KeepURLPasswords = bool(k) }

// ReplaceAttr changes or drops each attribute before it is kept and shown, as
// slog.HandlerOptions' does: groups are the attribute's groups, outermost
// first, and a zero Attr drops it. It is not called for the time, the level or
// the message.
func ReplaceAttr(replace func(groups []string, attr slog.Attr) slog.Attr) Option {
	return optionFunc(func(s *logline.Settings) { s.Replace = replace })
}

// FromEnv reads prefix_LOG_LEVEL, prefix_LOG_FORMAT and prefix_LOG_TIME, which
// win over the code. Without an environment option a handler reads LOG_LEVEL,
// LOG_FORMAT and LOG_TIME, as FromEnv("") does.
func FromEnv(prefix string) Option {
	return FromLookup(prefix, os.LookupEnv)
}

// FromLookup reads the variables FromEnv names through lookup, the process's
// environment left alone.
func FromLookup(prefix string, lookup func(name string) (string, bool)) Option {
	return optionFunc(func(s *logline.Settings) { s.Env = logline.Environment{Prefix: prefix, Lookup: lookup} })
}

// NoEnv reads no variable: what the code says is what the handler does.
const NoEnv = noEnv(true)

type noEnv bool

func (noEnv) Apply(s *logline.Settings) { s.Env = logline.Environment{} }

// AddSource adds where each line was logged, as slog's handlers spell it:
// source={"function":"main.run","file":"/app/main.go","line":42}, which a pretty
// line shows as source=app/main.go:42.
const AddSource = addSource(true)

type addSource bool

func (a addSource) Apply(s *logline.Settings) { s.AddSource = bool(a) }

type optionFunc func(*logline.Settings)

func (f optionFunc) Apply(s *logline.Settings) { f(s) }
