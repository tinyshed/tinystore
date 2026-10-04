package records

import (
	"log/slog"
	"os"
	"strings"
	"sync"
	"time"
)

// The environment wins over a handler's options, as in Bun and Python; a
// console the code turned off stays off.
const (
	envLevel  = "LOG_LEVEL"  // debug, info, warn or error
	envFormat = "LOG_FORMAT" // pretty, json or off
	envTime   = "LOG_TIME"   // clock, full or off
)

type unread struct {
	name, value, expected string
}

// fromEnvironment is settings with the environment over them, and what it could not read
func fromEnvironment(s handlerSettings) (handlerSettings, []unread) {
	var ignored []unread
	if text, ok := lookupLog(envLevel); ok {
		if level, read := readLevel(text); read {
			s.level = level
		} else {
			ignored = append(ignored, unread{envLevel, text, "debug, info, warn or error"})
		}
	}
	if text, ok := lookupLog(envFormat); ok {
		if console, read := readConsole(text); !read {
			ignored = append(ignored, unread{envFormat, text, "pretty, json or off"})
		} else if s.console != ConsoleOff {
			s.console = console
		}
	}
	if text, ok := lookupLog(envTime); ok {
		if shown, read := readConsoleTime(text); read {
			s.time = shown
		} else {
			ignored = append(ignored, unread{envTime, text, "clock, full or off"})
		}
	}
	return s, ignored
}

func lookupLog(name string) (string, bool) {
	text := strings.TrimSpace(os.Getenv(name))
	return text, text != ""
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

func readConsole(text string) (Console, bool) {
	switch strings.ToLower(text) {
	case "pretty":
		return ConsolePretty, true
	case "json":
		return ConsoleJSON, true
	case "off":
		return ConsoleOff, true
	}
	return 0, false
}

func readConsoleTime(text string) (ConsoleTime, bool) {
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

func sayIgnored(c *echo, ignored []unread, at time.Time) {
	for _, u := range ignored {
		if _, said := saidIgnored.LoadOrStore(u.name+"="+u.value, true); said {
			continue
		}
		level, message := slog.LevelWarn, u.name+" is ignored"
		c.write(&Record{
			At: at, Stream: "tinystore", Name: logName, Level: &level, Body: &message,
			Attrs: []Field{
				{Key: "value", Value: string(appendJSONString(nil, u.value))},
				{Key: "expected", Value: string(appendJSONString(nil, u.expected))},
			},
		})
	}
}
