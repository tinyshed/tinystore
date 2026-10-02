package main

import (
	"bytes"
	"context"
	"strings"
	"testing"
)

// stop asks the sidecar of a directory to stop, which it does as Ctrl+C would
// make it, giving the directory back; a directory nobody serves stays as it
// is, and one that holds no store is refused
func TestStopEndsTheServerOfADirectory(t *testing.T) {
	dir := t.TempDir()
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	serving := start(ctx, t, "--dir", dir, "--local", "--idle", "0")
	waitServe(t, dir, "", serving)

	var out, stderr bytes.Buffer
	if err := stopServing(t.Context(), []string{dir}, &out, &stderr); err != nil {
		t.Fatal(err, stderr.String())
	}
	if !strings.Contains(out.String(), "stopped") {
		t.Fatalf("stop said %q", out.String())
	}
	if err := ended(t, serving); err != nil {
		t.Fatalf("the stopped server ended with %v", err)
	}
	mustRelease(t, dir)

	out.Reset()
	if err := stopServing(t.Context(), []string{dir}, &out, &stderr); err != nil ||
		!strings.Contains(out.String(), "served by none") {
		t.Fatalf("a stop of a directory nobody serves: %q, %v", out.String(), err)
	}
	if err := stopServing(t.Context(), []string{t.TempDir()}, &out, &stderr); err == nil {
		t.Fatal("a stop of a directory that holds no store ran")
	}
}
