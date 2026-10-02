package main

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// status reads a directory as a store left it, its server beside it, and
// prints each engine's bytes and the server SERVE names, never its secret: for
// a person, and with --json for a script.
func TestStatusReadsADirectoryAndKeepsTheSecret(t *testing.T) {
	dir := t.TempDir()
	files := map[string]string{
		"LOCK":                "",
		"kv.db":               strings.Repeat("k", 4096),
		"kv.db-wal":           strings.Repeat("w", 100),
		"sql/app.db":          strings.Repeat("s", 8192),
		"blobs/blobs.db":      strings.Repeat("b", 4096),
		"blobs/objects/ab/cd": "hello",
		"server/SERVE": `{"protocol":1,"server":"v0.1.0","pid":42,"instance":"aW5zdA",` +
			`"secret":"the-secret-of-this-directory","endpoints":["unix:///run/tinystore.sock"]}`,
	}
	for name, body := range files {
		path := filepath.Join(dir, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
	}

	var out, stderr bytes.Buffer
	if err := status(t.Context(), []string{dir}, &out, &stderr); err != nil {
		t.Fatal(err, stderr.String())
	}
	for _, want := range []string{"SERVE names pid 42, which does not answer", "kv ", "sql/app", "blobs", "total"} {
		if !strings.Contains(out.String(), want) {
			t.Fatalf("status lacks %q:\n%s", want, out.String())
		}
	}
	if strings.Contains(out.String(), "the-secret-of-this-directory") {
		t.Fatalf("status printed SERVE's secret: %s", out.String())
	}

	out.Reset()
	if err := status(t.Context(), []string{"--dir", dir, "--json"}, &out, &stderr); err != nil {
		t.Fatal(err, stderr.String())
	}
	if strings.Contains(out.String(), "the-secret-of-this-directory") {
		t.Fatalf("status printed SERVE's secret: %s", out.String())
	}
	var found directoryStatus
	if err := json.Unmarshal(out.Bytes(), &found); err != nil {
		t.Fatal(err)
	}
	want := []fileStatus{{Name: "kv.db", Bytes: 4196}, {Name: "sql/app.db", Bytes: 8192}}
	if !found.Locked || len(found.Files) != 2 || found.Files[0] != want[0] || found.Files[1] != want[1] ||
		found.Blobs == nil || found.Blobs.Files != 2 || found.Blobs.Bytes != 4101 ||
		found.Server == nil || found.Server.Version != "v0.1.0" || found.Server.PID != 42 {
		t.Fatalf("status: %s", out.String())
	}

	if err := status(t.Context(), []string{t.TempDir()}, &out, &stderr); err == nil {
		t.Error("status of an empty directory ran")
	}
	if err := status(t.Context(), []string{"--dir", t.TempDir()}, &out, &stderr); err == nil {
		t.Error("status of a directory no store made ran")
	}
}
