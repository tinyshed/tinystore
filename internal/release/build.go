package main

import (
	"archive/tar"
	"archive/zip"
	"compress/gzip"
	"context"
	"crypto/sha256"
	"debug/buildinfo"
	"encoding/hex"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
)

// target is one platform a release builds for, named as each registry names it.
type target struct {
	goos, goarch string
	npmOS        string // process.platform
	npmCPU       string // process.arch
	wheel        string // the wheel's platform tag
}

// Go 1.27 runs on macOS 12 and later, and the binary is static, so one Linux
// wheel serves glibc and musl alike.
var targets = []target{
	{"linux", "amd64", "linux", "x64", "manylinux_2_17_x86_64.manylinux2014_x86_64.musllinux_1_1_x86_64"},
	{"linux", "arm64", "linux", "arm64", "manylinux_2_17_aarch64.manylinux2014_aarch64.musllinux_1_1_aarch64"},
	{"darwin", "amd64", "darwin", "x64", "macosx_12_0_x86_64"},
	{"darwin", "arm64", "darwin", "arm64", "macosx_12_0_arm64"},
	{"windows", "amd64", "win32", "x64", "win_amd64"},
	{"windows", "arm64", "win32", "arm64", "win_arm64"},
}

func (t target) executable() string {
	if t.goos == "windows" {
		return "tinystore.exe"
	}
	return "tinystore"
}

// binary is a built tinystore and the platform it runs on.
type binary struct {
	target
	path string
}

func buildBinaries(ctx context.Context, s settings) ([]binary, error) {
	var built []binary
	for _, t := range targets {
		path := filepath.Join(s.out, "bin", t.goos+"-"+t.goarch, t.executable())
		if err := buildOne(ctx, s, t, path); err != nil {
			return nil, err
		}
		if err := checkStamp(s, path); err != nil {
			return nil, err
		}
		built = append(built, binary{t, path})
	}
	return built, nil
}

// buildOne builds cmd/tinystore without cgo, its paths trimmed and its symbols
// stripped, so that one checkout builds the same bytes on any machine.
func buildOne(ctx context.Context, s settings, t target, path string) error {
	build := exec.CommandContext(ctx, "go", "build", "-trimpath", "-ldflags=-s -w", "-o", path, ".")
	build.Dir = filepath.Join(s.root, "cmd", "tinystore")
	build.Env = append(os.Environ(), "CGO_ENABLED=0", "GOOS="+t.goos, "GOARCH="+t.goarch, "GOWORK=off")
	if text, err := build.CombinedOutput(); err != nil {
		return fmt.Errorf("build %s/%s: %w\n%s", t.goos, t.goarch, err, text)
	}
	return nil
}

// checkStamp refuses a binary whose stamped version is not the release's: a
// checkout that is not the tag, or one with changes, stamps another.
func checkStamp(s settings, path string) error {
	info, err := buildinfo.ReadFile(path)
	if err != nil {
		return err
	}
	if !s.snapshot && info.Main.Version != s.tag {
		return fmt.Errorf("%s says it is %s, not %s: build a release from a clean checkout of its tag",
			path, info.Main.Version, s.tag)
	}
	return nil
}

// writeArchives writes each binary with the license as a .tar.gz, or a .zip
// for Windows, and SHA256SUMS over the archives.
func writeArchives(s settings, binaries []binary) error {
	var sums []string
	for _, b := range binaries {
		name := fmt.Sprintf("tinystore_%s_%s_%s", s.version, b.goos, b.goarch)
		archive := filepath.Join(s.out, name+".tar.gz")
		write := writeTarGz
		if b.goos == "windows" {
			archive, write = filepath.Join(s.out, name+".zip"), writeZip
		}
		files := map[string]string{b.executable(): b.path, "LICENSE": filepath.Join(s.root, "LICENSE")}
		if err := write(archive, files); err != nil {
			return err
		}
		sum, err := fileSHA256(archive)
		if err != nil {
			return err
		}
		sums = append(sums, sum+"  "+filepath.Base(archive))
	}
	return os.WriteFile(filepath.Join(s.out, "SHA256SUMS"), []byte(strings.Join(sums, "\n")+"\n"), 0o644)
}

func writeTarGz(path string, files map[string]string) error {
	out, err := os.Create(path)
	if err != nil {
		return err
	}
	defer out.Close()
	compressed := gzip.NewWriter(out)
	archive := tar.NewWriter(compressed)
	for _, name := range sortedKeys(files) {
		body, readErr := os.ReadFile(files[name])
		if readErr != nil {
			return readErr
		}
		header := &tar.Header{Name: name, Mode: int64(modeOf(name)), Size: int64(len(body)), Format: tar.FormatPAX}
		if err = archive.WriteHeader(header); err != nil {
			return err
		}
		if _, err = archive.Write(body); err != nil {
			return err
		}
	}
	if err = archive.Close(); err != nil {
		return err
	}
	if err = compressed.Close(); err != nil {
		return err
	}
	return out.Close()
}

func writeZip(path string, files map[string]string) error {
	out, err := os.Create(path)
	if err != nil {
		return err
	}
	defer out.Close()
	archive := zip.NewWriter(out)
	for _, name := range sortedKeys(files) {
		if err = addToZip(archive, name, files[name], modeOf(name)); err != nil {
			return err
		}
	}
	if err = archive.Close(); err != nil {
		return err
	}
	return out.Close()
}

func addToZip(archive *zip.Writer, name, from string, mode os.FileMode) error {
	body, err := os.ReadFile(from)
	if err != nil {
		return err
	}
	header := &zip.FileHeader{Name: name, Method: zip.Deflate}
	header.SetMode(mode)
	w, err := archive.CreateHeader(header)
	if err != nil {
		return err
	}
	_, err = w.Write(body)
	return err
}

// the executable is executable once unpacked, anywhere
func modeOf(name string) os.FileMode {
	if strings.HasPrefix(filepath.Base(name), "tinystore") {
		return 0o755
	}
	return 0o644
}

func fileSHA256(path string) (string, error) {
	f, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer f.Close()
	h := sha256.New()
	if _, err = io.Copy(h, f); err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

func sortedKeys(m map[string]string) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	slices.Sort(keys)
	return keys
}
