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
		"2026-09-26 12:00:01,500 ERROR [main] boom":                     new(slog.LevelError),
		"2026-09-21 21:09:00,391 - aiohttp.access - INFO - GET /health": new(slog.LevelInfo),
		"2026-09-11 13:14:49.437 UTC [48] LOG:  listening":              new(slog.LevelInfo),
		"2026-09-11 13:14:49.437 UTC [48] FATAL:  terminating":          &fatal,
		"2026/09/24 06:12:02.759278 [Warning] core: Xray started":       new(slog.LevelWarn),
		"1:M 11 Sep 2026 13:18:33.307 # WARNING Memory overcommit":      new(slog.LevelWarn),
		"2026/09/26 12:00:01 [error] 7#7: *1 open() failed":             new(slog.LevelError),
		"connection reset by peer":                                      nil,
		"no error occurred":                                             nil,
		"LOG rotated":                                                   nil,
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
