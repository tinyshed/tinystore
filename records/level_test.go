package records

import (
	"log/slog"
	"testing"
)

// the examples in the comment on levelOf, and lines that name no level
func TestLinesFindTheLevelWhereProgramsWriteIt(t *testing.T) {
	fatal := slog.LevelError + 4
	for line, want := range map[string]*slog.Level{
		`{"level":50,"time":1727300000121,"msg":"failed"}`:              new(slog.LevelError),
		`{"level":"warn","msg":"slow"}`:                                 new(slog.LevelWarn),
		`level=warn msg="slow request" ms=1200`:                         new(slog.LevelWarn),
		`ts=2026-09-26T12:00:01Z lvl=error msg=boom`:                    new(slog.LevelError),
		"E0926 12:00:01.000000  1 main.go:12] boom":                     new(slog.LevelError),
		"I20260923 00:47:32.100929   141 raft_server.h:60] ok":          new(slog.LevelInfo),
		"1:M 26 Sep 2026 12:00:01.500 # WARNING Memory overcommit":      new(slog.LevelWarn),
		"1:C 26 Sep 2026 12:00:01.500 * DB saved on disk":               new(slog.LevelInfo),
		"2026-09-26 12:00:01,500 ERROR [main] boom":                     new(slog.LevelError),
		"2026-09-21 21:09:00,391 - aiohttp.access - INFO - GET /health": new(slog.LevelInfo),
		"2026-09-11 13:14:49.437 UTC [48] LOG:  listening":              new(slog.LevelInfo),
		"2026-09-11 13:14:49.437 UTC [48] FATAL:  terminating":          &fatal,
		"2026/09/24 06:12:02.759278 [Warning] core: Xray started":       new(slog.LevelWarn),
		"2026/09/26 12:00:01 [error] 7#7: *1 open() failed":             new(slog.LevelError),
		"+0300 2026-09-26 12:00:01 \x1b[32mINFO\x1b[0m [main] started":  new(slog.LevelInfo),
		"[2026-09-26 12:00:03] \x1b[32minfo\x1b[39m (app): ready":       new(slog.LevelInfo),
		"2026-09-26 12:00:01.500 \x1b[36m    LOG\x1b[0m checkpoint":     new(slog.LevelInfo),
		"2026-09-26 12:00:01 INF Established secure connection":         new(slog.LevelInfo),
		"2026-09-26 12:00:01 WRN Failed TLS handshake":                  new(slog.LevelWarn),
		"logger=search namespace=default group=dashboards resource=folders t=2026-09-26T12:00:01Z level=info": new(
			slog.LevelInfo),
		"connection reset by peer":               nil,
		"no error occurred":                      nil,
		"LOG rotated":                            nil,
		"a \x1b[1mbold\x1b[0m word, then error":  nil,
		"1:M 26 Sep 2026 12:00:01.500 ? unknown": nil,
	} {
		records := []Field(nil)
		if fields, ok := exactFields(line); ok {
			records = fields
		}
		level, ok := levelOf(line, records)
		if ok != (want != nil) || (ok && level != *want) {
			t.Errorf("%q: level %v %v, want %v", line, level, ok, want)
		}
	}
}

// the examples in the comments on marked and escapeLength
func TestAColourMarksALevelAsBracketsDo(t *testing.T) {
	for _, word := range []struct {
		head       string
		start, end int
	}{{"[warn] x", 1, 5}, {"\x1b[33mwarn\x1b[0m x", 5, 9}, {"\x1b[36m    LOG\x1b[0m x", 9, 12}} {
		if !marked(word.head, word.start, word.end) {
			t.Errorf("%q: %q is not marked", word.head, word.head[word.start:word.end])
		}
	}
	if marked("a \x1b[1mbold word", 7, 11) {
		t.Error("a word after a colour and before none is marked")
	}
	if n := escapeLength("\x1b[1;32mINFO"); n != 7 {
		t.Errorf("a colour of %d bytes, want 7", n)
	}
	if n := escapeLength("\x1b[1;32"); n != 0 {
		t.Errorf("an unfinished colour of %d bytes, want 0", n)
	}
}
