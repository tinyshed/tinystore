package main

import (
	"archive/zip"
	"bytes"
	"crypto/sha256"
	"encoding/base64"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestAPlatformWheelNamesEachOfItsTags(t *testing.T) {
	pure := "Wheel-Version: 1.0\r\nGenerator: uv\r\nRoot-Is-Purelib: true\r\nTag: py3-none-any\r\n"
	want := "Wheel-Version: 1.0\nGenerator: uv\nRoot-Is-Purelib: false\n" +
		"Tag: py3-none-manylinux2014_x86_64\nTag: py3-none-musllinux_1_1_x86_64\n"
	if got := string(platformWHEEL([]byte(pure), "manylinux2014_x86_64.musllinux_1_1_x86_64")); got != want {
		t.Fatalf("WHEEL for a platform:\n%s\nwant\n%s", got, want)
	}
}

func TestAPreReleaseIsSpelledAsPyPIKeepsIt(t *testing.T) {
	for version, want := range map[string]string{
		"0.1.0": "0.1.0", "0.1.0-rc.1": "0.1.0rc1", "0.1.0-beta.2": "0.1.0b2", "0.1.0-alpha.3": "0.1.0a3",
	} {
		if got := pythonVersion(version); got != want {
			t.Errorf("%s: %s, want %s", version, got, want)
		}
	}
}

func TestATarballIsNamedAsNpmPackNamesIt(t *testing.T) {
	for name, want := range map[string]string{
		"@tinyshed/tinystore-linux-x64": "tinyshed-tinystore-linux-x64-0.1.0.tgz",
		"tinystore":                     "tinystore-0.1.0.tgz",
	} {
		if got := tarballName(name, "0.1.0"); got != want {
			t.Errorf("%s: %s, want %s", name, got, want)
		}
	}
}

// A platform wheel carries the binary, executable, and a RECORD whose every
// line is its file's hash and size, as an installer checks them.
func TestAPlatformWheelRecordsEveryFile(t *testing.T) {
	dir := t.TempDir()
	pure := filepath.Join(dir, "demo-0.1.0-py3-none-any.whl")
	writeTestWheel(t, pure, map[string]string{
		"tinystore/__init__.py":         "",
		"demo-0.1.0.dist-info/WHEEL":    "Root-Is-Purelib: true\nTag: py3-none-any\n",
		"demo-0.1.0.dist-info/RECORD":   "stale\n",
		"demo-0.1.0.dist-info/METADATA": "Name: demo\n",
	})
	executable := filepath.Join(dir, "tinystore")
	if err := os.WriteFile(executable, []byte("binary"), 0o755); err != nil {
		t.Fatal(err)
	}

	b := binary{target{goos: "linux", goarch: "amd64", wheel: "manylinux2014_x86_64"}, executable}
	if err := platformWheel(pure, b); err != nil {
		t.Fatal(err)
	}

	built, err := zip.OpenReader(filepath.Join(dir, "demo-0.1.0-py3-none-manylinux2014_x86_64.whl"))
	if err != nil {
		t.Fatal(err)
	}
	defer built.Close()
	files := map[string][]byte{}
	for _, f := range built.File {
		body, readErr := readZipFile(f)
		if readErr != nil {
			t.Fatal(readErr)
		}
		files[f.Name] = body
		if f.Name == "tinystore/bin/tinystore" && f.Mode().Perm() != 0o755 {
			t.Errorf("the binary's mode is %v", f.Mode())
		}
	}
	checkRecord(t, files, "demo-0.1.0.dist-info/RECORD")
}

func checkRecord(t *testing.T, files map[string][]byte, record string) {
	t.Helper()
	listed := map[string]bool{}
	for line := range strings.Lines(string(files[record])) {
		fields := strings.Split(strings.TrimSpace(line), ",")
		listed[fields[0]] = true
		if fields[0] == record {
			continue
		}
		sum := sha256.Sum256(files[fields[0]])
		want := fmt.Sprintf("sha256=%s", base64.RawURLEncoding.EncodeToString(sum[:]))
		if fields[1] != want || fields[2] != fmt.Sprint(len(files[fields[0]])) {
			t.Errorf("RECORD says %q for %s", line, fields[0])
		}
	}
	for name := range files {
		if !listed[name] {
			t.Errorf("RECORD does not name %s", name)
		}
	}
	if !bytes.Contains(files["demo-0.1.0.dist-info/WHEEL"], []byte("Root-Is-Purelib: false")) {
		t.Error("the platform wheel still says it is pure")
	}
}

func writeTestWheel(t *testing.T, path string, files map[string]string) {
	t.Helper()
	out, err := os.Create(path)
	if err != nil {
		t.Fatal(err)
	}
	archive := zip.NewWriter(out)
	for name, body := range files {
		w, createErr := archive.Create(name)
		if createErr != nil {
			t.Fatal(createErr)
		}
		if _, createErr = w.Write([]byte(body)); createErr != nil {
			t.Fatal(createErr)
		}
	}
	if err = archive.Close(); err != nil {
		t.Fatal(err)
	}
	if err = out.Close(); err != nil {
		t.Fatal(err)
	}
}
