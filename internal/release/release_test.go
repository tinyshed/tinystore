package main

import (
	"archive/tar"
	"archive/zip"
	"bytes"
	"compress/gzip"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"maps"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"
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

func TestAPyprojectIsStampedWithTheReleasesVersion(t *testing.T) {
	text := "[project]\nname = \"demo\"\nversion = \"0.0.0\"  # a release stamps its own into what it builds\n" +
		"\n[tool.ruff]\ntarget-version = \"py312\"\n"
	want := "[project]\nname = \"demo\"\nversion = \"0.1.0rc1\"\n\n[tool.ruff]\ntarget-version = \"py312\"\n"
	got, err := stampPyproject([]byte(text), "0.1.0rc1")
	if err != nil || string(got) != want {
		t.Fatalf("stamped: %q, %v\nwant %q", got, err, want)
	}
	if _, err = stampPyproject([]byte(text+"version = \"1\"\n"), "0.1.0"); err == nil {
		t.Error("a pyproject with two version lines was stamped")
	}
}

// The repository's manifest says 0.0.0; what npm gets says the release, and
// holds the files the manifest names, its command executable.
func TestTheSDKPackageCarriesTheReleasesVersion(t *testing.T) {
	root := t.TempDir()
	files := map[string]string{
		"sdk/js/package.json": `{"name": "@tinyshed/tinystore", "version": "0.0.0", "scripts": {"test": "bun test"},
			"devDependencies": {"typescript": "^7.0.2"}, "engines": {"node": ">=22"},
			"exports": {".": {"bun": "./src/index.ts", "types": "./dist/index.d.ts", "default": "./dist/index.js"}},
			"bin": {"tinystore": "bin/tinystore.js"}, "files": ["src", "bin", "README.md"]}`,
		"sdk/js/src/index.ts":     "export {}\n",
		"sdk/js/bin/tinystore.js": "#!/usr/bin/env node\n",
		"sdk/js/README.md":        "# tinystore\n",
		"sdk/js/test/kv.test.ts":  "a test npm does not get\n",
		"LICENSE":                 "Apache-2.0\n",
	}
	for name, body := range files {
		path := filepath.Join(root, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	out := t.TempDir()
	s := settings{tag: "v0.1.0-rc.1", version: "0.1.0-rc.1", root: root}
	platforms := map[string]string{"@tinyshed/tinystore-linux-x64": "0.1.0-rc.1"}
	if err := writeSDKPackage(s, out, platforms); err != nil {
		t.Fatal(err)
	}

	manifest := packageManifest(t, filepath.Join(out, "tinyshed-tinystore-0.1.0-rc.1.tgz"))
	if manifest["version"] != "0.1.0-rc.1" || manifest["scripts"] != nil || manifest["devDependencies"] != nil {
		t.Errorf("the package's manifest: %v", manifest)
	}
	if optional, _ := manifest["optionalDependencies"].(map[string]any); optional["@tinyshed/tinystore-linux-x64"] != "0.1.0-rc.1" {
		t.Errorf("the package names its platforms as %v", manifest["optionalDependencies"])
	}
	text := packageFile(t, filepath.Join(out, "tinyshed-tinystore-0.1.0-rc.1.tgz"), "package/package.json")
	conditions := regexp.MustCompile(`"(bun|types|default)":`).FindAllStringSubmatch(text, -1)
	if len(conditions) != 3 || conditions[0][1] != "bun" || conditions[1][1] != "types" ||
		conditions[2][1] != "default" || !strings.Contains(text, `">=22"`) {
		t.Errorf("the package's manifest changed its exports' order or its spelling:\n%s", text)
	}
	want := map[string]int64{
		"package/package.json": 0o644, "package/LICENSE": 0o644, "package/README.md": 0o644,
		"package/src/index.ts": 0o644, "package/bin/tinystore.js": 0o755,
	}
	if got := packageModes(t, filepath.Join(out, "tinyshed-tinystore-0.1.0-rc.1.tgz")); !maps.Equal(got, want) {
		t.Errorf("the package holds %v, want %v", got, want)
	}

	if err := os.Remove(filepath.Join(root, "sdk", "js", "README.md")); err != nil {
		t.Fatal(err)
	}
	if err := writeSDKPackage(s, out, platforms); err == nil {
		t.Error("a package whose files name what is not there was written")
	}
}

// packageFile is a file's text in a package's tarball
func packageFile(t *testing.T, tarball, name string) string {
	t.Helper()
	file, err := os.Open(tarball)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	unzipped, err := gzip.NewReader(file)
	if err != nil {
		t.Fatal(err)
	}
	entries := tar.NewReader(unzipped)
	for {
		header, err := entries.Next()
		if err != nil {
			t.Fatalf("no %s in %s: %v", name, tarball, err)
		}
		if header.Name == name {
			text, err := io.ReadAll(entries)
			if err != nil {
				t.Fatal(err)
			}
			return string(text)
		}
	}
}

// packageModes is each file's mode in a package's tarball, by its name
func packageModes(t *testing.T, tarball string) map[string]int64 {
	t.Helper()
	file, err := os.Open(tarball)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	unzipped, err := gzip.NewReader(file)
	if err != nil {
		t.Fatal(err)
	}
	modes := map[string]int64{}
	entries := tar.NewReader(unzipped)
	for {
		header, err := entries.Next()
		if errors.Is(err, io.EOF) {
			return modes
		}
		if err != nil {
			t.Fatal(err)
		}
		modes[header.Name] = header.Mode
	}
}

func packageManifest(t *testing.T, tarball string) map[string]any {
	t.Helper()
	file, err := os.Open(tarball)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	unzipped, err := gzip.NewReader(file)
	if err != nil {
		t.Fatal(err)
	}
	entries := tar.NewReader(unzipped)
	for {
		header, err := entries.Next()
		if err != nil {
			t.Fatalf("no package/package.json in %s: %v", tarball, err)
		}
		if header.Name == "package/package.json" {
			var manifest map[string]any
			if err = json.NewDecoder(entries).Decode(&manifest); err != nil {
				t.Fatal(err)
			}
			return manifest
		}
	}
}

func TestATarballIsNamedAsNpmPackNamesIt(t *testing.T) {
	for name, want := range map[string]string{
		"@tinyshed/tinystore-linux-x64": "tinyshed-tinystore-linux-x64-0.1.0.tgz",
		"@tinyshed/tinystore":           "tinyshed-tinystore-0.1.0.tgz",
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
		"tinystore/":                    "",
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
		if f.Flags&0x8 != 0 {
			t.Errorf("%s gives its sizes after its bytes, in a data descriptor PyPI refuses", f.Name)
		}
		if !f.Modified.Equal(time.Date(1980, 1, 1, 0, 0, 0, 0, time.UTC)) {
			t.Errorf("%s was modified %v, not on the first day a zip can name", f.Name, f.Modified)
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
