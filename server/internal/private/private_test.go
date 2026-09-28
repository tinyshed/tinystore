package private

import (
	"os"
	"path/filepath"
	"testing"
)

// a directory made private is its owner's alone, and so is one that was there
// before, with a file in it
func TestADirectoryIsItsOwnersAlone(t *testing.T) {
	made := filepath.Join(t.TempDir(), "server")
	if err := Dir(made); err != nil {
		t.Fatal(err)
	}
	if err := Check(made); err != nil {
		t.Fatal(err)
	}

	there := filepath.Join(t.TempDir(), "server")
	if err := os.MkdirAll(there, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(there, "SERVE"), []byte("{}"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := Dir(there); err != nil {
		t.Fatal(err)
	}
	if err := Check(there); err != nil {
		t.Fatal(err)
	}
}

// a file where the directory belongs is refused, not made private
func TestAFileIsNoPrivateDirectory(t *testing.T) {
	file := filepath.Join(t.TempDir(), "server")
	if err := os.WriteFile(file, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := Dir(file); err == nil {
		t.Fatal("a file taken for a private directory")
	}
}
