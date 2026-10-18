package core

import (
	"encoding/json"
	"os"
	"sync"
	"time"
)

// What the SDK does in the background, a worker's loop or an instrument's
// send, has no caller to tell when it fails, so it says so in TinyStore's own
// lines on standard error: once, again when the failure changes or ten
// minutes on, and once more when the work recovers.

// quiet is how long a failure repeated is counted rather than said again.
const quiet = 10 * time.Minute

var saying sync.Mutex

// SayOwn says one of TinyStore's own lines, as a logger of the stream
// tinystore prints JSON: its level, its message and its fields in pairs.
func SayOwn(level, message string, fields ...any) {
	line := map[string]any{
		"time":   time.Now().UTC().Format("2006-01-02T15:04:05.000Z"),
		"level":  level,
		"stream": "tinystore",
		"msg":    message,
	}
	for i := 0; i+1 < len(fields); i += 2 {
		if key, ok := fields[i].(string); ok {
			value := fields[i+1]
			if err, isErr := value.(error); isErr {
				value = err.Error()
			}
			line[key] = value
		}
	}
	text, err := json.Marshal(line)
	if err != nil {
		return
	}
	saying.Lock()
	defer saying.Unlock()
	_, _ = os.Stderr.Write(append(text, '\n'))
}

// FailureLog is a background work's failures, said as the core's are:
//
//	00:00  disk full        → WARN "background work failed" failures=1
//	00:01…00:09 the same    → counted
//	00:10  disk full        → WARN failures=11
//	00:11  success          → INFO "background work recovered" failures=11
type FailureLog struct {
	Work     string
	mu       sync.Mutex
	last     string
	saidAt   time.Time
	failures int
}

// Failed counts a failure, and says it unless the same was said within the quiet span.
func (f *FailureLog) Failed(err error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.failures++
	message := err.Error()
	if message == f.last && time.Since(f.saidAt) < quiet {
		return
	}
	SayOwn("WARN", "background work failed", "work", f.Work, "error", message, "failures", f.failures)
	f.last, f.saidAt = message, time.Now()
}

// Succeeded says the work recovered, once, after failures.
func (f *FailureLog) Succeeded() {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.failures > 0 {
		SayOwn("INFO", "background work recovered", "work", f.Work, "failures", f.failures)
	}
	f.last, f.failures = "", 0
}
