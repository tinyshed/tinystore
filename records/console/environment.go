package console

import (
	"log/slog"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/tinyshed/tinystore/records/internal/logline"
)

// environment is where a handler reads LOG_LEVEL, LOG_FORMAT and LOG_TIME,
// which win over its options, as in Bun and Python; a console the code turned
// off stays off.
type environment struct {
	prefix string
	lookup func(string) (string, bool) // nil reads nothing
}

func (e environment) apply(s *settings) { s.env = e }

// the variables' names after a prefix and its underscore
const (
	envLevel  = "LOG_LEVEL"  // debug, info, warn or error
	envFormat = "LOG_FORMAT" // pretty, json or off
	envTime   = "LOG_TIME"   // clock, full or off
)

func (e environment) name(variable string) string {
	if e.prefix == "" {
		return variable
	}
	return e.prefix + "_" + variable
}

func (e environment) value(variable string) (string, bool) {
	if e.lookup == nil {
		return "", false
	}
	text, _ := e.lookup(e.name(variable))
	text = strings.TrimSpace(text)
	return text, text != ""
}

type unread struct {
	name, value, expected string
}

// overSettings is s with the environment over it, and what it could not read
func (e environment) overSettings(s settings) (settings, []unread) {
	var ignored []unread
	if text, ok := e.value(envLevel); ok {
		if level, read := readLevel(text); read {
			s.level = level
		} else {
			ignored = append(ignored, unread{e.name(envLevel), text, "debug, info, warn or error"})
		}
	}
	if text, ok := e.value(envFormat); ok {
		if format, read := readFormat(text); !read {
			ignored = append(ignored, unread{e.name(envFormat), text, "pretty, json or off"})
		} else if s.format != Off {
			s.format = format
		}
	}
	if text, ok := e.value(envTime); ok {
		if shown, read := readTime(text); read {
			s.time = shown
		} else {
			ignored = append(ignored, unread{e.name(envTime), text, "clock, full or off"})
		}
	}
	return s, ignored
}

func readLevel(text string) (slog.Level, bool) {
	switch strings.ToLower(text) {
	case "debug":
		return slog.LevelDebug, true
	case "info":
		return slog.LevelInfo, true
	case "warn", "warning":
		return slog.LevelWarn, true
	case "error":
		return slog.LevelError, true
	}
	return 0, false
}

func readFormat(text string) (Format, bool) {
	switch strings.ToLower(text) {
	case "pretty":
		return Pretty, true
	case "json":
		return JSON, true
	case "off":
		return Off, true
	}
	return 0, false
}

func readTime(text string) (Time, bool) {
	switch strings.ToLower(text) {
	case "clock":
		return TimeClock, true
	case "full":
		return TimeFull, true
	case "off":
		return TimeOff, true
	}
	return 0, false
}

// saidIgnored keeps a value said to be ignored from being said again by the next logger
var saidIgnored sync.Map

func sayIgnored(echo *logline.Echo, ignored []unread, at time.Time) {
	for _, u := range ignored {
		if _, said := saidIgnored.LoadOrStore(u.name+"="+u.value, true); said {
			continue
		}
		level, message := slog.LevelWarn, u.name+" is ignored"
		echo.Write(&logline.Line{
			At: at, Stream: "tinystore", Name: logline.LogName, Level: &level, Body: &message,
			Attrs: []logline.Field{
				{Key: "value", Value: string(logline.AppendString(nil, u.value))},
				{Key: "expected", Value: string(logline.AppendString(nil, u.expected))},
			},
		})
	}
}

// defaultEnvironment is what a handler reads without an environment option
var defaultEnvironment = environment{lookup: os.LookupEnv}
