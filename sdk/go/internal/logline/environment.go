package logline

import (
	"log/slog"
	"os"
	"strings"
	"sync"
	"time"
)

// Environment is where a handler reads LOG_LEVEL, LOG_CONSOLE_LEVEL,
// LOG_FORMAT and LOG_TIME, each of which wins over its own option, as in Bun,
// Python and Rust; a console the code turned off stays off.
type Environment struct {
	Prefix string
	Lookup func(string) (string, bool) // nil reads nothing
}

// DefaultEnvironment is what a handler reads without an environment option.
var DefaultEnvironment = Environment{Lookup: os.LookupEnv}

// the variables' names after a prefix and its underscore
const (
	envLevel        = "LOG_LEVEL"         // trace, debug, info, warn or error
	envConsoleLevel = "LOG_CONSOLE_LEVEL" // the same, of what is printed alone
	envFormat       = "LOG_FORMAT"        // pretty, json or off
	envTime         = "LOG_TIME"          // clock, full or off
)

func (e Environment) name(variable string) string {
	if e.Prefix == "" {
		return variable
	}
	return e.Prefix + "_" + variable
}

func (e Environment) value(variable string) (string, bool) {
	if e.Lookup == nil {
		return "", false
	}
	text, _ := e.Lookup(e.name(variable))
	text = strings.TrimSpace(text)
	return text, text != ""
}

// Unread is a variable whose value a handler could not read.
type Unread struct {
	Name, Value, Expected string
}

// over is s with the environment over it, what it gave as LOG_ spells it,
// and what it could not read. LOG_CONSOLE_LEVEL is one process's, which no
// setting of the config log holds, so LOG_ spells it nowhere.
func (e Environment) over(s Settings) (Settings, map[string]string, []Unread) {
	var ignored []Unread
	spelled := map[string]string{}
	if text, ok := e.value(envLevel); ok {
		if level, read := ReadLevel(text); read {
			s.Level = level
			spelled[e.name(envLevel)] = LevelName(level)
		} else {
			ignored = append(ignored, Unread{e.name(envLevel), text, "trace, debug, info, warn or error"})
		}
	}
	if text, ok := e.value(envConsoleLevel); ok {
		if level, read := ReadLevel(text); read {
			s.Printed = level
		} else {
			ignored = append(ignored, Unread{e.name(envConsoleLevel), text, "trace, debug, info, warn or error"})
		}
	}
	if text, ok := e.value(envFormat); ok {
		if format, read := readFormat(text); !read {
			ignored = append(ignored, Unread{e.name(envFormat), text, "pretty, json or off"})
		} else if s.Format != Off {
			s.Format = format
			spelled[e.name(envFormat)] = strings.ToLower(text)
		}
	}
	if text, ok := e.value(envTime); ok {
		if shown, read := ReadTime(text); read {
			s.Time = shown
			spelled[e.name(envTime)] = strings.ToLower(text)
		} else {
			ignored = append(ignored, Unread{e.name(envTime), text, "clock, full or off"})
		}
	}
	return s, spelled, ignored
}

// LevelTrace is the level below debug that other programs write, which a
// handler at trace takes with every other.
const LevelTrace = slog.Level(-8)

// LevelNames are the levels a logger's config allows, lowest first.
var LevelNames = []string{"trace", "debug", "info", "warn", "error"}

// LevelName is the name of the lowest level at or above level: trace, debug,
// info, warn or error.
func LevelName(level slog.Level) string {
	for _, name := range LevelNames {
		if named, _ := ReadLevel(name); named >= level {
			return name
		}
	}
	return "error"
}

// ReadLevel reads a level's name as LOG_LEVEL spells it.
func ReadLevel(text string) (slog.Level, bool) {
	switch strings.ToLower(text) {
	case "trace":
		return LevelTrace, true
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

// ReadTime reads how a pretty line shows its time as LOG_TIME spells it.
func ReadTime(text string) (Time, bool) {
	switch strings.ToLower(text) {
	case "clock":
		return Clock, true
	case "full":
		return Full, true
	case "off":
		return NoTime, true
	}
	return 0, false
}

// saidIgnored keeps a value said to be ignored from being said again by the next logger
var saidIgnored sync.Map

// SayIgnored says each variable a handler could not read once a process,
// as one of TinyStore's own lines on the handler's console.
func SayIgnored(echo *Echo, ignored []Unread) {
	for _, u := range ignored {
		if _, said := saidIgnored.LoadOrStore(u.Name+"="+u.Value, true); said {
			continue
		}
		level, message := slog.LevelWarn, u.Name+" is ignored"
		echo.Write(&Line{
			At: time.Now(), Stream: "tinystore", Name: LogName, Level: &level, Body: &message,
			Attrs: []Field{
				{Key: "value", Value: string(AppendString(nil, u.Value))},
				{Key: "expected", Value: string(AppendString(nil, u.Expected))},
			},
		})
	}
}
