package main

import (
	"archive/zip"
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/base64"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// writeWheels builds sdk/python's pure wheel and sdist with uv, then a wheel a
// platform that carries its binary as tinystore/bin/tinystore, where the SDK
// looks first. The pure wheel stays for a platform without its own, whose SDK
// finds tinystore on PATH.
func writeWheels(ctx context.Context, s settings, binaries []binary) error {
	dir := filepath.Join(s.out, "pypi")
	build := exec.CommandContext(ctx, "uv", "build", "--out-dir", dir)
	build.Dir = filepath.Join(s.root, "sdk", "python")
	if text, err := build.CombinedOutput(); err != nil {
		return fmt.Errorf("uv build: %w\n%s", err, text)
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
