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
//	1:M 26 Sep 2026 12:00:01.500 # WARNING …         Redis's mark after its time
//	2026-09-26 12:00:01,500 ERROR [main] …           log4j, Python, Postgres: the first level word
//	2026/09/26 06:12:02 [Warning] core: started      a level in brackets or in colour, in any case
func levelOf(text string, fields []Field) (slog.Level, bool) {
	if fields != nil {
		return fieldLevel(fields)
	}
	if level, ok := glogLevel(text); ok {
		return level, true
	}
	if level, ok := redisLevel(text); ok {
		return level, true
	}
	firstLine, _, _ := strings.Cut(text, "\n")
	if level, ok := logfmtLevel(firstLine); ok {
		return level, true
	}
	return wordLevel(firstLine[:min(len(firstLine), levelReach)])
}

// levelReach is how far into a line a level word is looked for
const levelReach = 96

// levelNames are the names programs give their levels, lower case; zerolog's
// console writes the three letters
var levelNames = map[string]slog.Level{
	"trace": slog.LevelDebug - 4, "trc": slog.LevelDebug - 4,
	"debug": slog.LevelDebug, "dbg": slog.LevelDebug,
	"info": slog.LevelInfo, "inf": slog.LevelInfo, "notice": slog.LevelInfo, "log": slog.LevelInfo,
	"warn": slog.LevelWarn, "wrn": slog.LevelWarn, "warning": slog.LevelWarn,
	"error": slog.LevelError, "err": slog.LevelError, "severe": slog.LevelError,
	"fatal": slog.LevelError + 4, "ftl": slog.LevelError + 4, "critical": slog.LevelError + 4,
	"crit": slog.LevelError + 4, "panic": slog.LevelError + 4, "alert": slog.LevelError + 4,
	"emerg": slog.LevelError + 4,
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

// redisStamp is the time Redis writes after its pid and role
var redisStamp = patterns("D N Y h:m:s")[0]

// redisLevel is the mark Redis writes after its time: . and - for debug, *
// for notice, # for warning
//
//	1:M 26 Sep 2026 12:00:01.500 * Ready to accept connections   → info
func redisLevel(text string) (slog.Level, bool) {
	role := strings.IndexByte(text, ':')
	if role < 1 || role+3 > len(text) || !allDigits(text[:role]) || strings.IndexByte("MCSX", text[role+1]) < 0 ||
		text[role+2] != ' ' {
		return 0, false
	}
	found, ok := readStamp(text, role+3, redisStamp, 0)
	mark := found.end + 1
	if !ok || mark+1 >= len(text) || text[found.end] != ' ' || text[mark+1] != ' ' {
		return 0, false
	}
	level, ok := map[byte]slog.Level{
		'.': slog.LevelDebug, '-': slog.LevelDebug, '*': slog.LevelInfo, '#': slog.LevelWarn,
	}[text[mark]]
	return level, ok
}

func allDigits(text string) bool {
	for i := range len(text) {
		if !isDigit(text[i]) {
			return false
		}
	}
	return text != ""
}

// logfmtLevel is the value of a level= or lvl= pair
func logfmtLevel(line string) (slog.Level, bool) {
	for _, key := range []string{"level=", "lvl="} {
		at := strings.Index(line, key)
		if at < 0 || (at > 0 && line[at-1] != ' ') {
			continue
		}
		value := line[at+len(key):]
		if end := strings.IndexAny(value, " \t"); end >= 0 {
			value = value[:end]
		}
		level, ok := levelNames[strings.ToLower(strings.Trim(value, `"`))]
		return level, ok
	}
	return 0, false
}

// wordLevel is the first level a line names: a word in capitals, or a name
// in brackets or in colour in any case; Postgres's LOG counts only with its
// colon, unless it is in colour
func wordLevel(head string) (slog.Level, bool) {
	for start := 0; start < len(head); {
		end := start
		for end < len(head) && isLetter(head[end]) {
			end++
		}
		if end == start {
			start += max(1, escapeLength(head[start:]))
			continue
		}
		word := head[start:end]
		level, named := levelNames[strings.ToLower(word)]
		if named && (marked(head, start, end) || isLevelWord(word, head[end:])) {
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

// marked is a word in brackets, or between colour codes, spaces aside:
//
//	[warn]      \x1b[33mwarn\x1b[0m      \x1b[36m    LOG\x1b[0m
func marked(head string, start, end int) bool {
	if start > 0 && head[start-1] == '[' && end < len(head) && head[end] == ']' {
		return true
	}
	before := strings.TrimRight(head[:start], " ")
	escape := strings.LastIndexByte(before, 0x1b)
	return escape >= 0 && escapeLength(before[escape:]) == len(before)-escape && escapeLength(head[end:]) > 0
}

// escapeLength is how long the terminal control sequence text begins with
// is, a colour's included; zero when it begins with none
//
//	\x1b[1;32m → 7
func escapeLength(text string) int {
	if len(text) < 3 || text[0] != 0x1b || text[1] != '[' {
		return 0
	}
	for i := 2; i < len(text); i++ {
		switch c := text[i]; {
		case c >= 0x40 && c <= 0x7e:
			return i + 1
		case c < 0x20 || c > 0x3f:
			return 0
		}
	}
	return 0
}

func isLetter(c byte) bool {
	return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')
}
