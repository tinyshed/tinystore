package main

import (
	"archive/zip"
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/base64"
	"fmt"
	"io"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
)

// writeWheels builds sdk/python's pure wheel and sdist with uv, from a copy
// stamped with the release's version, then a wheel a platform that carries its
// binary as tinystore/bin/tinystore, where the SDK looks first. The pure wheel
// stays for a platform without its own, whose SDK finds tinystore on PATH.
func writeWheels(ctx context.Context, s settings, binaries []binary) error {
	dir := filepath.Join(s.out, "pypi")
	source, err := stampedPython(s)
	if err != nil {
		return err
	}
	defer func() { _ = os.RemoveAll(source) }()
	build := exec.CommandContext(ctx, "uv", "build", "--out-dir", dir)
	build.Dir = source
	if text, failed := build.CombinedOutput(); failed != nil {
		return fmt.Errorf("uv build: %w\n%s", failed, text)
	}
	pure, err := filepath.Glob(filepath.Join(dir, "*-py3-none-any.whl"))
	if err != nil {
		return err
	}
	if len(pure) != 1 {
		return fmt.Errorf("uv build left %d pure wheels in %s, not one", len(pure), dir)
	}
	for _, b := range binaries {
		if err = platformWheel(pure[0], b); err != nil {
			return err
		}
	}
	return nil
}

// stampedPython copies sdk/python into a directory of its own, the version in
// its pyproject the release's as PyPI spells it; what only development made, a
// virtual environment and caches, stays behind.
func stampedPython(s settings) (string, error) {
	copied, err := os.MkdirTemp("", "tinystore-python-")
	if err != nil {
		return "", err
	}
	if err = copyStamped(filepath.Join(s.root, "sdk", "python"), copied, pythonVersion(s.version)); err != nil {
		_ = os.RemoveAll(copied)
		return "", err
	}
	return copied, nil
}

// copyStamped copies a directory's files within it, never through a link out
// of it, the pyproject at its top stamped with version.
func copyStamped(source, target, version string) error {
	from, err := os.OpenRoot(source)
	if err != nil {
		return err
	}
	defer from.Close()
	to, err := os.OpenRoot(target)
	if err != nil {
		return err
	}
	defer to.Close()

	return fs.WalkDir(from.FS(), ".", func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if entry.IsDir() {
			if path != "." && developmentOnly[entry.Name()] {
				return fs.SkipDir
			}
			return to.MkdirAll(path, 0o755)
		}
		body, err := from.ReadFile(path)
		if err == nil && path == "pyproject.toml" {
			body, err = stampPyproject(body, version)
		}
		if err != nil {
			return err
		}
		return to.WriteFile(path, body, 0o644)
	})
}

// the directories of sdk/python that only its development made
var developmentOnly = map[string]bool{
	".venv": true, ".bin": true, "__pycache__": true, ".pytest_cache": true, ".ruff_cache": true, "dist": true,
}

// stampPyproject writes a version into a pyproject's one version line:
//
//	version = "0.0.0"  # a release stamps its own …   →   version = "0.1.0rc1"
func stampPyproject(text []byte, version string) ([]byte, error) {
	line := regexp.MustCompile(`(?m)^version = "[^"\r\n]*"[^\r\n]*`)
	if found := len(line.FindAll(text, -1)); found != 1 {
		return nil, fmt.Errorf("sdk/python/pyproject.toml has %d version lines, not one", found)
	}
	return line.ReplaceAllLiteral(text, []byte(`version = "`+version+`"`)), nil
}

// platformWheel copies the pure wheel with the binary added, WHEEL naming the
// platform, and RECORD written again over every file:
//
//	tinyshed_tinystore-0.1.0-py3-none-any.whl
//	  → tinyshed_tinystore-0.1.0-py3-none-win_amd64.whl
func platformWheel(pure string, b binary) error {
	source, err := zip.OpenReader(pure)
	if err != nil {
		return err
	}
	defer source.Close()
	out := strings.TrimSuffix(pure, "any.whl") + b.wheel + ".whl"

	w := &wheelWriter{record: &bytes.Buffer{}}
	if w.file, err = os.Create(out); err != nil {
		return err
	}
	defer w.file.Close()
	w.archive = zip.NewWriter(w.file)
	info, err := copyWheel(w, source.File, b)
	if err != nil {
		return err
	}
	if err = w.add("tinystore/bin/"+b.executable(), b.path, 0o755); err != nil {
		return err
	}
	return w.finish(info + "/RECORD")
}

// copyWheel copies every file but RECORD, with WHEEL rewritten for the
// platform, and returns the name of the .dist-info directory.
func copyWheel(w *wheelWriter, files []*zip.File, b binary) (string, error) {
	info := ""
	for _, f := range files {
		dir, base := filepath.ToSlash(filepath.Dir(f.Name)), filepath.Base(f.Name)
		if strings.HasSuffix(dir, ".dist-info") {
			info = dir
			if base == "RECORD" {
				continue
			}
		}
		body, err := readZipFile(f)
		if err != nil {
			return "", err
		}
		if strings.HasSuffix(dir, ".dist-info") && base == "WHEEL" {
			body = platformWHEEL(body, b.wheel)
		}
		if err = w.write(f.Name, body, f.Mode().Perm()); err != nil {
			return "", err
		}
	}
	if info == "" {
		return "", fmt.Errorf("a wheel with no .dist-info")
	}
	return info, nil
}

// platformWHEEL says the wheel is not pure, and names a tag for each of the
// platform's, a compressed tag being several joined by dots:
//
//	Root-Is-Purelib: true    →  Root-Is-Purelib: false
//	Tag: py3-none-any        →  Tag: py3-none-win_amd64
func platformWHEEL(body []byte, platform string) []byte {
	var lines []string
	for line := range strings.Lines(string(body)) {
		line = strings.TrimRight(line, "\r\n")
		switch {
		case strings.HasPrefix(line, "Root-Is-Purelib:"):
			lines = append(lines, "Root-Is-Purelib: false")
		case strings.HasPrefix(line, "Tag:"):
			for tag := range strings.SplitSeq(platform, ".") {
				lines = append(lines, "Tag: py3-none-"+tag)
			}
		default:
			lines = append(lines, line)
		}
	}
	return []byte(strings.Join(lines, "\n") + "\n")
}

// wheelWriter writes a wheel's files and the RECORD of them.
type wheelWriter struct {
	file    *os.File
	archive *zip.Writer
	record  *bytes.Buffer
}

func (w *wheelWriter) add(name, from string, mode os.FileMode) error {
	body, err := os.ReadFile(from)
	if err != nil {
		return err
	}
	return w.write(name, body, mode)
}

func (w *wheelWriter) write(name string, body []byte, mode os.FileMode) error {
	header := &zip.FileHeader{Name: name, Method: zip.Deflate}
	if mode == 0 {
		mode = 0o644
	}
	header.SetMode(mode)
	out, err := w.archive.CreateHeader(header)
	if err != nil {
		return err
	}
	if _, err = out.Write(body); err != nil {
		return err
	}
	sum := sha256.Sum256(body)
	fmt.Fprintf(w.record, "%s,sha256=%s,%d\n", name, base64.RawURLEncoding.EncodeToString(sum[:]), len(body))
	return nil
}

// finish writes RECORD, which names itself without a hash, and closes the wheel
func (w *wheelWriter) finish(record string) error {
	fmt.Fprintf(w.record, "%s,,\n", record)
	header := &zip.FileHeader{Name: record, Method: zip.Deflate}
	header.SetMode(0o644)
	out, err := w.archive.CreateHeader(header)
	if err != nil {
		return err
	}
	if _, err = out.Write(w.record.Bytes()); err != nil {
		return err
	}
	if err = w.archive.Close(); err != nil {
		return err
	}
	return w.file.Close()
}

func readZipFile(f *zip.File) ([]byte, error) {
	r, err := f.Open()
	if err != nil {
		return nil, err
	}
	defer r.Close()
	return io.ReadAll(r)
}
