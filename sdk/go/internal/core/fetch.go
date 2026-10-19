package core

import (
	"archive/tar"
	"archive/zip"
	"bufio"
	"bytes"
	"compress/gzip"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path"
	"path/filepath"
	"regexp"
	"runtime"
	"strings"
	"sync"
)

// A program that depends on a released SDK gets the core and the tinystore
// binary of the same release without installing anything: the first open
// that finds neither fetches the release's archive for this platform from
// GitHub, checks it against the release's SHA256SUMS, and keeps its bin/ and
// lib/ in the user's cache, where every later open finds them.
//
// TINYSTORE_RELEASES names another place that serves the release's files by
// their tag, a mirror, or "off" to never fetch; TINYSTORE_CACHE another
// directory to keep them in.
const releases = "https://github.com/tinyshed/tinystore/releases/download"

// build is what one fetch reads: the SDK's version, without its v, and this
// platform's target as the release names its archives.
type build struct {
	version string
	target  string
}

var fetching sync.Mutex

// pseudo is a version Go makes up for a commit no tag names: no release has it.
var pseudo = regexp.MustCompile(`\d{14}-[0-9a-f]{12}$`)

// fetchedBinary is the release's tinystore binary, fetched once.
func fetchedBinary(ctx context.Context) (string, error) {
	dir, err := fetched(ctx, build{Version, target()})
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "bin", executableName()), nil
}

// fetchedLibrary is the release's core library, fetched once.
func fetchedLibrary(ctx context.Context) (string, error) {
	dir, err := fetched(ctx, build{Version, target()})
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "lib", libraryName()), nil
}

// fetched is the directory holding the release's bin/ and lib/, fetched
// into the cache unless a process before this one did.
func fetched(ctx context.Context, r build) (string, error) {
	base := os.Getenv("TINYSTORE_RELEASES")
	if base == "off" {
		return "", errors.New("TINYSTORE_RELEASES is off")
	}
	if base == "" {
		base = releases
	}
	if r.version == "0.0.0" || pseudo.MatchString(r.version) {
		return "", fmt.Errorf("tinyshed.org/tinystore %s is no release, so none can be fetched", r.version)
	}
	if r.target == "" {
		return "", fmt.Errorf("no release is built for %s/%s", runtime.GOOS, runtime.GOARCH)
	}
	cache, err := cacheDir()
	if err != nil {
		return "", err
	}
	dir := filepath.Join(cache, r.version, r.target)
	fetching.Lock()
	defer fetching.Unlock()
	if whole(dir) {
		return dir, nil
	}
	if err := download(ctx, base, r, dir); err != nil {
		return "", fmt.Errorf("fetch tinystore %s for %s: %w", r.version, r.target, err)
	}
	return dir, nil
}

func cacheDir() (string, error) {
	if dir := os.Getenv("TINYSTORE_CACHE"); dir != "" {
		return dir, nil
	}
	dir, err := os.UserCacheDir()
	if err != nil {
		return "", fmt.Errorf("no cache directory to keep the release in, set TINYSTORE_CACHE: %w", err)
	}
	return filepath.Join(dir, "tinystore"), nil
}

// whole says the directory holds both files a store needs, which it does only
// once a fetch renamed it into place.
func whole(dir string) bool {
	for _, file := range []string{filepath.Join("bin", executableName()), filepath.Join("lib", libraryName())} {
		if _, err := os.Stat(filepath.Join(dir, file)); err != nil {
			return false
		}
	}
	return true
}

// download fetches the archive and its sum, unpacks bin/ and lib/ beside dir
// and renames them into place, so that a fetch cut short leaves nothing
// another process could take for the release.
func download(ctx context.Context, base string, r build, dir string) error {
	tag := "v" + r.version
	name := archiveName(r)
	sums, err := get(ctx, base+"/"+tag+"/SHA256SUMS")
	if err != nil {
		return err
	}
	want, err := sumOf(sums, name)
	if err != nil {
		return err
	}
	archive, err := get(ctx, base+"/"+tag+"/"+name)
	if err != nil {
		return err
	}
	if got := sha256.Sum256(archive); hex.EncodeToString(got[:]) != want {
		return fmt.Errorf("%s is not the file SHA256SUMS names", name)
	}
	if err := os.MkdirAll(filepath.Dir(dir), 0o755); err != nil {
		return err
	}
	staged, err := os.MkdirTemp(filepath.Dir(dir), ".fetch-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(staged)
	if err := unpack(archive, strings.HasSuffix(name, ".zip"), staged); err != nil {
		return err
	}
	if !whole(staged) {
		return fmt.Errorf("%s holds no bin/%s and lib/%s", name, executableName(), libraryName())
	}
	if err := os.Rename(staged, dir); err != nil && !whole(dir) {
		return err
	}
	return nil
}

// the largest file a fetch reads: an archive is tens of megabytes
const largestDownload = 512 << 20

func get(ctx context.Context, url string) ([]byte, error) {
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, err
	}
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("GET %s: %s", url, response.Status)
	}
	return io.ReadAll(io.LimitReader(response.Body, largestDownload))
}

// sumOf reads a file's hash from SHA256SUMS, as sha256sum writes it:
// "<hex>  <name>" a line.
func sumOf(sums []byte, name string) (string, error) {
	lines := bufio.NewScanner(strings.NewReader(string(sums)))
	for lines.Scan() {
		hash, file, ok := strings.Cut(lines.Text(), "  ")
		if ok && file == name {
			return hash, nil
		}
	}
	return "", fmt.Errorf("SHA256SUMS names no %s", name)
}

// unpack writes the archive's bin/ and lib/ under dir, its top folder taken
// off, and nothing else of it.
func unpack(archive []byte, isZip bool, dir string) error {
	write := func(name string, mode os.FileMode, body io.Reader) error {
		_, inner, ok := strings.Cut(path.Clean(name), "/")
		if !ok || !(strings.HasPrefix(inner, "bin/") || strings.HasPrefix(inner, "lib/")) {
			return nil
		}
		target := filepath.Join(dir, filepath.FromSlash(inner))
		if !strings.HasPrefix(target, dir+string(filepath.Separator)) {
			return fmt.Errorf("the archive's %s leaves its folder", name)
		}
		if err := os.MkdirAll(filepath.Dir(target), 0o755); err != nil {
			return err
		}
		file, err := os.OpenFile(target, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, mode)
		if err != nil {
			return err
		}
		if _, err := io.Copy(file, body); err != nil {
			file.Close()
			return err
		}
		return file.Close()
	}
	if isZip {
		return unzip(archive, write)
	}
	return untar(archive, write)
}

func untar(archive []byte, write func(string, os.FileMode, io.Reader) error) error {
	unzipped, err := gzip.NewReader(bytes.NewReader(archive))
	if err != nil {
		return err
	}
	files := tar.NewReader(unzipped)
	for {
		header, err := files.Next()
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return err
		}
		if header.Typeflag == tar.TypeReg {
			if err := write(header.Name, fileMode(header.Name), files); err != nil {
				return err
			}
		}
	}
}

func unzip(archive []byte, write func(string, os.FileMode, io.Reader) error) error {
	files, err := zip.NewReader(bytes.NewReader(archive), int64(len(archive)))
	if err != nil {
		return err
	}
	for _, entry := range files.File {
		if entry.FileInfo().IsDir() {
			continue
		}
		body, err := entry.Open()
		if err != nil {
			return err
		}
		err = write(entry.Name, fileMode(entry.Name), body)
		body.Close()
		if err != nil {
			return err
		}
	}
	return nil
}

// fileMode runs what bin/ holds; an archive's own modes are not trusted
func fileMode(name string) os.FileMode {
	if strings.Contains(name, "/bin/") {
		return 0o755
	}
	return 0o644
}

func archiveName(r build) string {
	if strings.HasPrefix(r.target, "win32-") {
		return "tinystore-" + r.version + "-" + r.target + ".zip"
	}
	return "tinystore-" + r.version + "-" + r.target + ".tar.gz"
}

// target is this platform as the release names its archives, or "" for one
// it does not build.
func target() string {
	switch runtime.GOOS + "/" + runtime.GOARCH {
	case "linux/amd64":
		return "linux-x64" + muslSuffix()
	case "linux/arm64":
		return "linux-arm64" + muslSuffix()
	case "darwin/arm64":
		return "darwin-arm64"
	case "darwin/amd64":
		return "darwin-x64"
	case "windows/amd64":
		return "win32-x64"
	}
	return ""
}

// muslSuffix names musl's build where musl's loader is the system's, as on Alpine.
func muslSuffix() string {
	if found, _ := filepath.Glob("/lib/ld-musl-*.so.1"); len(found) > 0 {
		return "-musl"
	}
	return ""
}

func executableName() string {
	if runtime.GOOS == "windows" {
		return "tinystore.exe"
	}
	return "tinystore"
}

func libraryName() string {
	switch runtime.GOOS {
	case "windows":
		return "tinystore_ffi.dll"
	case "darwin":
		return "libtinystore_ffi.dylib"
	}
	return "libtinystore_ffi.so"
}
