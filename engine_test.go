package tinystore

import (
	"bytes"
	"errors"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestClaimGivesEachNameToOneEngine(t *testing.T) {
	dir := t.TempDir()
	store := openTestStore(t, dir, Options{})

	for _, name := range []string{"metrics.db", "records.db", "blobs/", "sql/app.db"} {
		path, _, err := store.Claim(name)
		if err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		if want := filepath.Join(dir, filepath.FromSlash(strings.TrimSuffix(name, "/"))); path != want {
			t.Fatalf("%s is at %s, want %s", name, path, want)
		}
		if info, statErr := os.Stat(path); strings.HasSuffix(name, "/") && (statErr != nil || !info.IsDir()) {
			t.Fatalf("the directory %s was not made: %v", name, statErr)
		}
		if _, _, err = store.Claim(name); !errors.Is(err, ErrInUse) {
			t.Fatalf("%s claimed twice: %v", name, err)
		}
	}
}

func TestReleaseLetsAnEngineThatFailedOpenAgain(t *testing.T) {
	store := openTestStore(t, t.TempDir(), Options{})
	_, release, err := store.Claim("metrics.db")
	if err != nil {
		t.Fatal(err)
	}
	release()
	if _, _, err = store.Claim("metrics.db"); err != nil {
		t.Fatalf("claim after release: %v", err)
	}
}

func TestClaimRefusesNamesOutsideTheStore(t *testing.T) {
	store := openTestStore(t, t.TempDir(), Options{})
	for _, name := range []string{"../elsewhere.db", "/etc/passwd", "", "LOCK", "sql/../../x.db"} {
		if _, _, err := store.Claim(name); !errors.Is(err, ErrInvalid) {
			t.Errorf("%q: %v", name, err)
		}
	}
}

func TestLoggerNamesTheEngineAndNowIsTheStoresClock(t *testing.T) {
	var out bytes.Buffer
	at := time.Date(2026, 9, 24, 12, 0, 0, 0, time.UTC)
	store := openTestStore(t, t.TempDir(), Options{
		Logger: slog.New(slog.NewTextHandler(&out, nil)),
		Clock:  func() time.Time { return at },
	})

	store.Logger("metrics").Info("opened")
	if !strings.Contains(out.String(), "msg=opened engine=metrics") {
		t.Fatalf("log: %s", out.String())
	}
	if !store.Now().Equal(at) {
		t.Fatalf("now %v, want %v", store.Now(), at)
	}
}
