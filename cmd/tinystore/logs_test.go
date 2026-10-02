package main

import (
	"bytes"
	"context"
	"encoding/json"
	"os"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/server/reach"
	"github.com/tinyshed/tinystore/server/wire"
)

// servedStore serves a new store in a directory as its sidecar, until the
// test ends, and hands back a connection to it
func servedStore(t *testing.T) (string, *reach.Conn) {
	t.Helper()
	dir := t.TempDir()
	ctx, cancel := context.WithCancel(context.Background())
	serving := start(ctx, t, "--dir", dir, "--local", "--idle", "0")
	t.Cleanup(func() {
		cancel()
		if err := ended(t, serving); err != nil {
			t.Error(err)
		}
	})
	waitServe(t, dir, "", serving)
	conn, err := reach.Found(t.Context(), dir, "tinystore-test")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	return dir, conn
}

// appendLines appends log lines to a stream, a level and a body each, a
// millisecond apart from now
func appendLines(t *testing.T, conn *reach.Conn, stream string, lines ...string) {
	t.Helper()
	var batch []wire.Record
	now := time.Now()
	for i := 0; i+1 < len(lines); i += 2 {
		batch = append(batch, logLine(t, stream, now.Add(time.Duration(i)*time.Millisecond), lines[i], lines[i+1]))
	}
	appendRecords(t, conn, batch...)
}

func logLine(t *testing.T, stream string, at time.Time, level, body string) wire.Record {
	t.Helper()
	n, err := levelOf(level)
	if err != nil {
		t.Fatal(err)
	}
	return wire.Record{At: at.UnixNano(), Stream: stream, Name: "log", Level: &n, Body: &body}
}

func appendRecords(t *testing.T, conn *reach.Conn, batch ...wire.Record) {
	t.Helper()
	if _, err := conn.Call(t.Context(), wire.RecordsAppend, wire.RecordsBatch{Records: batch}); err != nil {
		t.Fatal(err)
	}
}

// expectMessages checks the messages of JSON console lines, in their order
func expectMessages(t *testing.T, lines string, want ...string) {
	t.Helper()
	var found []string
	for line := range strings.Lines(strings.TrimSpace(lines)) {
		var shown struct {
			Msg string `json:"msg"`
		}
		if err := json.Unmarshal([]byte(line), &shown); err != nil {
			t.Fatalf("a line that is not a JSON object: %q", line)
		}
		found = append(found, shown.Msg)
	}
	if !slices.Equal(found, want) {
		t.Fatalf("printed %q, want %q", found, want)
	}
}

// waitPrinted waits for a follower to have printed a message as often as asked
func waitPrinted(t *testing.T, followed *lockedBuffer, msg string, times int) {
	t.Helper()
	for deadline := time.Now().Add(10 * time.Second); strings.Count(followed.String(), `"`+msg+`"`) < times; {
		if time.Now().After(deadline) {
			t.Fatalf("logs -f printed %q fewer than %d times:\n%s", msg, times, followed.String())
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// logs prints a store's last records, oldest first, from a level up, the
// number -n asks for, and with -f those that arrive after them: one sent a
// little late too, each once and two alike twice, and none from before the
// first it printed
func TestLogsPrintTheLastRecordsAndFollowTheNext(t *testing.T) {
	dir, conn := servedStore(t)
	base := time.Now().Add(-time.Minute)
	appendRecords(t, conn, logLine(t, "api", base, "info", "started"),
		logLine(t, "api", base.Add(time.Millisecond), "warn", "slow request"),
		logLine(t, "api", base.Add(2*time.Millisecond), "error", "GET /users/0 500"))

	var out, stderr bytes.Buffer
	if err := logs(t.Context(), []string{dir, "--json"}, &out, &stderr); err != nil {
		t.Fatal(err, stderr.String())
	}
	expectMessages(t, out.String(), "started", "slow request", "GET /users/0 500")
	out.Reset()
	if err := logs(t.Context(), []string{"--level", "warn", "-n", "1", "--json", dir}, &out, &stderr); err != nil {
		t.Fatal(err)
	}
	expectMessages(t, out.String(), "GET /users/0 500")

	followed := &lockedBuffer{}
	ctx, stop := context.WithCancel(t.Context())
	var following sync.WaitGroup
	following.Go(func() {
		if err := logs(ctx, []string{dir, "-f", "-n", "2", "--json", "--stream", "api"}, followed, &stderr); err != nil {
			t.Error(err)
		}
	})
	appendRecords(t, conn, logLine(t, "api", base.Add(5*time.Second), "info", "a new request"),
		logLine(t, "worker", base.Add(5*time.Second), "info", "another stream's line"))
	waitPrinted(t, followed, "a new request", 1)
	appendRecords(t, conn, logLine(t, "api", base.Add(time.Millisecond/2), "info", "before the first printed"),
		logLine(t, "api", base.Add(3*time.Second), "info", "a late line"),
		logLine(t, "api", base.Add(6*time.Second), "info", "twice"),
		logLine(t, "api", base.Add(6*time.Second), "info", "twice"))
	waitPrinted(t, followed, "twice", 2)
	time.Sleep(followEvery + followEvery/2) // one read more, which prints none of them again
	stop()
	following.Wait()
	expectMessages(t, followed.String(), "slow request", "GET /users/0 500", "a new request", "a late line", "twice",
		"twice")
}

// a read of a directory that holds no store is refused, and makes none
func TestLogsOfADirectoryWithoutAStoreMakeNone(t *testing.T) {
	dir := t.TempDir()
	var out, stderr bytes.Buffer
	if err := logs(t.Context(), []string{dir}, &out, &stderr); err == nil || !strings.Contains(err.Error(),
		"holds no store") {
		t.Fatalf("logs of an empty directory: %v", err)
	}
	if entries, err := os.ReadDir(dir); err != nil || len(entries) != 0 {
		t.Fatalf("logs left %v in the directory: %v", entries, err)
	}
}
