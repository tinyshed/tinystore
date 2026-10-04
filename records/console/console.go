// Package console is TinyStore's logger without the store: a log/slog handler
// that writes each line as it is logged, pretty for a person at a terminal and
// one JSON object a line for a collector, as the Bun and Python loggers write
// theirs. It links neither SQLite nor zstd. A records store's Handler writes
// the same lines, takes the same options, and keeps every line too.
//
//	slog.SetDefault(slog.New(console.Handler("agent", console.Redact(console.Secrets...))))
//
//	11:02:11.123 WARN  agent  slow request  ms=1200 password=[redacted]
package console

import (
	"fmt"
	"io"
	"log/slog"
	"os"
	"time"

	"github.com/tinyshed/tinystore/records/internal/logline"
)

func init() {
	logline.NewHandler = func(stream string, keeper logline.Keeper, options any) slog.Handler {
		given, ok := options.([]Option)
		if !ok {
			panic(fmt.Sprintf("records/console: a keeper's options of %T, not []console.Option", options))
		}
		return newHandler(stream, keeper, given)
	}
}

// Handler writes a logger's lines to standard error, and keeps none.
func Handler(stream string, options ...Option) slog.Handler {
	return newHandler(stream, nil, options)
}

// Option changes a logger's lines: on the console, and in the store when the
// handler is a records store's.
type Option interface{ apply(*settings) }

type settings struct {
	format           Format
	time             Time
	hideStream       bool
	to               io.Writer // standard error when nil
	level            slog.Leveler
	redact           []string
	keepURLPasswords bool
	replace          func(groups []string, attr slog.Attr) slog.Attr
	env              environment
	addSource        bool
	zone             *time.Location // a test's, in place of the machine's own
}

// Format is how the console writes a line. Without one, lines are pretty on a
// terminal and JSON otherwise.
type Format int

const (
	Pretty Format = iota + 1 // 11:02:11.123 INFO  api  started  port=3000
	JSON                     // one object a line, for a collector
	Off                      // nothing: a store's handler still keeps each line
)

func (f Format) apply(s *settings) { s.format = f }

// Time is how a pretty line shows its time; a JSON line always has it.
type Time int

const (
	TimeClock Time = iota + 1 // 11:02:11.123, the default
	TimeFull                  // 2026-10-02 11:02:11.123 +03:00
	TimeOff                   // none, where docker or journald stamp each line
)

func (t Time) apply(s *settings) { s.time = t }

// HideStream leaves the stream out of a pretty line; a JSON line and the store keep it.
const HideStream = hiddenStream(true)

type hiddenStream bool

func (h hiddenStream) apply(s *settings) { s.hideStream = bool(h) }

// To writes the lines to w instead of standard error.
func To(w io.Writer) Option {
	return optionFunc(func(s *settings) { s.to = w })
}

// Level keeps lines from level up; without it every line is kept.
func Level(level slog.Leveler) Option {
	return optionFunc(func(s *settings) { s.level = level })
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
	return optionFunc(func(s *settings) { s.redact = append(s.redact, names...) })
}

// Secrets is what Redact takes to hide the usual secrets. It hides a counter
// named like one too, token_count among them.
var Secrets = []string{
	"password", "passwd", "passphrase", "secret", "token", "credential", "credentials", "authorization", "cookie",
	"api key", "private key", "secret key", "access key", "signing key", "encryption key",
	"connection string", "dsn",
}

// KeepURLPasswords leaves the password of a URL inside a value as it is. Without
// it, postgres://ann:hunter2@db/app is kept as postgres://ann:[redacted]@db/app.
const KeepURLPasswords = keptURLPasswords(true)

type keptURLPasswords bool

func (k keptURLPasswords) apply(s *settings) { s.keepURLPasswords = bool(k) }

// ReplaceAttr changes or drops each attribute before it is kept and shown, as
// slog.HandlerOptions' does: groups are the attribute's groups, outermost
// first, and a zero Attr drops it. It is not called for the time, the level or
// the message.
func ReplaceAttr(replace func(groups []string, attr slog.Attr) slog.Attr) Option {
	return optionFunc(func(s *settings) { s.replace = replace })
}

// FromEnv reads prefix_LOG_LEVEL, prefix_LOG_FORMAT and prefix_LOG_TIME, which
// win over the code. Without an environment option a handler reads LOG_LEVEL,
// LOG_FORMAT and LOG_TIME, as FromEnv("") does.
func FromEnv(prefix string) Option {
	return environment{prefix: prefix, lookup: os.LookupEnv}
}

// FromLookup reads the variables FromEnv names through lookup, the process's
// environment left alone.
func FromLookup(prefix string, lookup func(name string) (string, bool)) Option {
	return environment{prefix: prefix, lookup: lookup}
}

// NoEnv reads no variable: what the code says is what the handler does.
const NoEnv = noEnv(true)

type noEnv bool

func (noEnv) apply(s *settings) { s.env = environment{} }

// AddSource adds where each line was logged, as slog's handlers spell it:
// source={"function":"main.run","file":"/app/main.go","line":42}, which a pretty
// line shows as source=app/main.go:42.
const AddSource = addSource(true)

type addSource bool

func (a addSource) apply(s *settings) { s.addSource = bool(a) }

type optionFunc func(*settings)

func (f optionFunc) apply(s *settings) { f(s) }
