package core

import (
	"archive/tar"
	"archive/zip"
	"bytes"
	"compress/gzip"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

// archived is a release's archive as tools/release.ts packs it: one top
// folder holding bin/, lib/ and the license.
func archived(t *testing.T, r build, isZip bool) []byte {
	t.Helper()
	top := strings.TrimSuffix(strings.TrimSuffix(archiveName(r), ".zip"), ".tar.gz")
	files := map[string]string{
		top + "/bin/" + executableName(): "binary",
		top + "/lib/" + libraryName():    "library",
		top + "/lib/tinystore.node":      "addon",
		top + "/LICENSE":                 "license",
	}
	var out bytes.Buffer
	if isZip {
		w := zip.NewWriter(&out)
		for name, body := range files {
			f, _ := w.Create(name)
			_, _ = f.Write([]byte(body))
		}
		_ = w.Close()
		return out.Bytes()
	}
	gz := gzip.NewWriter(&out)
	w := tar.NewWriter(gz)
	for name, body := range files {
		_ = w.WriteHeader(&tar.Header{Name: name, Mode: 0o600, Size: int64(len(body)), Typeflag: tar.TypeReg})
		_, _ = w.Write([]byte(body))
	}
	_ = w.Close()
	_ = gz.Close()
	return out.Bytes()
}

// serving answers a release's two files by their tag, counting the archive's requests.
func serving(t *testing.T, r build, archive []byte, sum string) *atomic.Int32 {
	t.Helper()
	var asked atomic.Int32
	name := archiveName(r)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		switch req.URL.Path {
		case "/v" + r.version + "/SHA256SUMS":
			_, _ = w.Write([]byte("0000  other.tar.gz\n" + sum + "  " + name + "\n"))
		case "/v" + r.version + "/" + name:
			asked.Add(1)
			_, _ = w.Write(archive)
		default:
			http.NotFound(w, req)
		}
	}))
	t.Cleanup(server.Close)
	t.Setenv("TINYSTORE_RELEASES", server.URL)
	t.Setenv("TINYSTORE_CACHE", t.TempDir())
	return &asked
}

func sumOfBytes(b []byte) string {
	sum := sha256.Sum256(b)
	return hex.EncodeToString(sum[:])
}

func TestAReleaseIsFetchedOnceAndKeptWithItsBinAndLib(t *testing.T) {
	for _, r := range []build{{"0.1.0-rc.6", "linux-x64"}, {"0.1.0-rc.6", "win32-x64"}} {
		archive := archived(t, r, strings.HasPrefix(r.target, "win32-"))
		asked := serving(t, r, archive, sumOfBytes(archive))

		dir, err := fetched(context.Background(), r)
		if err != nil {
			t.Fatal(err)
		}
		again, err := fetched(context.Background(), r)
		if err != nil {
			t.Fatal(err)
		}

		if dir != again || asked.Load() != 1 {
			t.Fatalf("%s: fetched into %s then %s, the archive asked for %d times", r.target, dir, again, asked.Load())
		}
		body, _ := os.ReadFile(filepath.Join(dir, "bin", executableName()))
		if string(body) != "binary" {
			t.Fatalf("%s: bin holds %q", r.target, body)
		}
		if _, err := os.Stat(filepath.Join(dir, "LICENSE")); err == nil {
			t.Fatalf("%s: what is not bin/ or lib/ was kept", r.target)
		}
	}
}

func TestAnArchiveItsSumDoesNotNameIsRefusedAndNothingKept(t *testing.T) {
	r := build{"0.1.0-rc.6", "linux-x64"}
	serving(t, r, archived(t, r, false), strings.Repeat("ab", 32))

	_, err := fetched(context.Background(), r)

	if err == nil || !strings.Contains(err.Error(), "not the file SHA256SUMS names") {
		t.Fatalf("got %v", err)
	}
	if whole(filepath.Join(os.Getenv("TINYSTORE_CACHE"), r.version, r.target)) {
		t.Fatal("a refused archive was kept")
	}
}

// contract: WIRE-11
func TestAFetchAFirstOpenGaveUpOnStopsAndKeepsNothing(t *testing.T) {
	r := build{"0.1.0-rc.6", "linux-x64"}
	archive := archived(t, r, false)
	name := archiveName(r)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		switch req.URL.Path {
		case "/v" + r.version + "/SHA256SUMS":
			_, _ = w.Write([]byte(sumOfBytes(archive) + "  " + name + "\n"))
		case "/v" + r.version + "/" + name:
			// an archive slow to come, as a first open on a slow network meets it
			select {
			case <-req.Context().Done():
			case <-time.After(3 * time.Second):
				_, _ = w.Write(archive)
			}
		default:
			http.NotFound(w, req)
		}
	}))
	t.Cleanup(server.Close)
	t.Setenv("TINYSTORE_RELEASES", server.URL)
	cache := t.TempDir()
	t.Setenv("TINYSTORE_CACHE", cache)
	link := NewLink(func(ctx context.Context) (*Connection, error) {
		if _, err := fetched(ctx, r); err != nil {
			return nil, err
		}
		return nil, errors.New("fetched, with no server to dial")
	})
	ctx, cancel := context.WithTimeout(context.Background(), 200*time.Millisecond)
	defer cancel()
	started := time.Now()
	if _, err := link.Connection(ctx); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("got %v, want the deadline", err)
	}
	link.Close()
	if took := time.Since(started); took > time.Second {
		t.Fatalf("the close waited %s for a fetch that nobody waits for", took)
	}
	if whole(filepath.Join(cache, r.version, r.target)) {
		t.Fatal("a fetch cut short was kept")
	}
	if staged, _ := filepath.Glob(filepath.Join(cache, r.version, ".fetch-*")); len(staged) > 0 {
		t.Fatalf("a fetch cut short left %v", staged)
	}
}

func TestNoReleaseIsFetchedForACheckoutOrWhenTurnedOff(t *testing.T) {
	r := build{"0.1.0-rc.6", "linux-x64"}
	archive := archived(t, r, false)
	asked := serving(t, r, archive, sumOfBytes(archive))

	for _, version := range []string{"0.0.0", "0.0.0-20261019185109-0f8c38492ed1"} {
		if _, err := fetched(context.Background(), build{version, r.target}); err == nil {
			t.Fatalf("%s fetched", version)
		}
	}
	t.Setenv("TINYSTORE_RELEASES", "off")
	if _, err := fetched(context.Background(), r); err == nil {
		t.Fatal("fetched while off")
	}
	if asked.Load() != 0 {
		t.Fatal("the archive was asked for")
	}
}
