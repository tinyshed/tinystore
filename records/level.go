package records

import (
	"log/slog"
	"strconv"
	"strings"
)

// levelOf finds a line's level where the programs writing lines put it:
//
//	{"level":50,…}                                   pino and bunyan: 50 → error
//	{"level":"warn",…}   level=warn msg=…            JSON and logfmt, by name
//	E0926 12:00:01.000000  1 main.go:12] …           glog's first letter
//	2026-09-26 12:00:01,500 ERROR [main] …           log4j, Python, Postgres: the first level word
//	2026/09/26 06:12:02 [Warning] core: started      a level in brackets, in any case
func levelOf(text string, fields []Field) (slog.Level, bool) {
	if fields != nil {
		return fieldLevel(fields)
	}
	if level, ok := glogLevel(text); ok {
		return level, true
	}
	head := text[:min(len(text), levelReach)]
	if level, ok := logfmtLevel(head); ok {
		return level, true
	}
	return wordLevel(head)
}

// levelReach is how far into a line a level is looked for
const levelReach = 96

// levelNames are the names programs give their levels, lower case
var levelNames = map[string]slog.Level{
	"trace": slog.LevelDebug - 4, "debug": slog.LevelDebug,
	"info": slog.LevelInfo, "notice": slog.LevelInfo, "log": slog.LevelInfo,
	"warn": slog.LevelWarn, "warning": slog.LevelWarn,
	"error": slog.LevelError, "err": slog.LevelError, "severe": slog.LevelError,
	"fatal": slog.LevelError + 4, "critical": slog.LevelError + 4, "crit": slog.LevelError + 4,
	"panic": slog.LevelError + 4, "alert": slog.LevelError + 4, "emerg": slog.LevelError + 4,
}

// the keys a JSON line gives its level under
var levelKeys = []string{"level", "lvl", "severity", "levelname", "log.level"}

func fieldLevel(fields []Field) (slog.Level, bool) {
	for _, field := range fields {
		for _, key := range levelKeys {
			if field.Key == key {
				return valueLevel(field.Value)
			}
		}
	}
	return 0, false
}

// valueLevel reads a level written as a name, or as pino's number:
//
//	10 trace   20 debug   30 info   40 warn   50 error   60 fatal
func valueLevel(value string) (slog.Level, bool) {
	if name, quoted := unquote(value); quoted {
		level, ok := levelNames[strings.ToLower(name)]
		return level, ok
	}
	number, err := strconv.Atoi(value)
	if err != nil {
		return 0, false
	}
	return pinoLevel(number), true
}

func pinoLevel(number int) slog.Level {
	switch {
	case number < 20:
		return slog.LevelDebug - 4
	case number < 30:
		return slog.LevelDebug
	case number < 40:
		return slog.LevelInfo
	case number < 50:
		return slog.LevelWarn
	case number < 60:
		return slog.LevelError
	}
	return slog.LevelError + 4
}

// glogLevel is the letter glog begins a line with, before the digits of its date
func glogLevel(text string) (slog.Level, bool) {
	if len(text) < 2 || !isDigit(text[1]) {
		return 0, false
	}
	level, ok := map[byte]slog.Level{
		'I': slog.LevelInfo, 'W': slog.LevelWarn, 'E': slog.LevelError, 'F': slog.LevelError + 4,
	}[text[0]]
	return level, ok
}

// logfmtLevel is the value of a level= or lvl= pair
func logfmtLevel(head string) (slog.Level, bool) {
	for _, key := range []string{"level=", "lvl="} {
		at := strings.Index(head, key)
		if at < 0 || (at > 0 && head[at-1] != ' ') {
			continue
		}
		value := head[at+len(key):]
		if end := strings.IndexAny(value, " \t"); end >= 0 {
			value = value[:end]
		}
		level, ok := levelNames[strings.ToLower(strings.Trim(value, `"`))]
		return level, ok
	}
	return 0, false
}

// wordLevel is the first level a line names: a word in capitals, or a name
// in brackets in any case; Postgres's LOG counts only with its colon
func wordLevel(head string) (slog.Level, bool) {
	for start := 0; start < len(head); {
		end := start
		for end < len(head) && isLetter(head[end]) {
			end++
		}
		if end == start {
			start++
			continue
		}
		word := head[start:end]
		bracketed := start > 0 && head[start-1] == '[' && end < len(head) && head[end] == ']'
		if level, ok := levelNames[strings.ToLower(word)]; ok && (bracketed || isLevelWord(word, head[end:])) {
			return level, true
		}
		start = end
	}
	return 0, false
}

func isLevelWord(word, after string) bool {
	if word != strings.ToUpper(word) || len(word) < 3 {
		return false
	}
	return word != "LOG" || strings.HasPrefix(after, ":")
}

func isLetter(c byte) bool {
	return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')
}
