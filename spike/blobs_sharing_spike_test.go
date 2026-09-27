package spike

import (
	"bytes"
	"io"
	"os"
	"path/filepath"
	"testing"
)

// TestBlobsSharing opens a 1 MiB file, reads half of it, has its name removed,
// replaced or linked from under the reader, and reports on this system whether
// that was allowed, whether the reader still read every byte, and whether the
// name was free at once; the reader opened through os.Open or through an
// os.Root, which on Windows share their deletion differently
func TestBlobsSharing(t *testing.T) {
	kvMeasuring(t)
	for _, reader := range []string{"os.Open", "Root.Open"} {
		for _, change := range []string{
			"os.Remove", "Root.Remove", "Root.Rename over it", "os.Rename over it",
			"Root.Link, then Root.Remove",
		} {
			blobsShare(t, reader, change)
		}
	}
}

func blobsShare(t *testing.T, reader, change string) {
	t.Helper()
	dir := blobsDir(t)
	root, err := os.OpenRoot(dir)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = root.Close() }()
	data, other := blobsBytes(10, 1<<20), blobsBytes(11, 1<<20)
	if err = os.WriteFile(filepath.Join(dir, "a"), data, 0o600); err != nil {
		t.Fatal(err)
	}
	if err = os.WriteFile(filepath.Join(dir, "b"), other, 0o600); err != nil {
		t.Fatal(err)
	}
	var file *os.File
	if reader == "os.Open" {
		file, err = os.Open(filepath.Join(dir, "a"))
	} else {
		file, err = root.Open("a")
	}
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = file.Close() }()
	half := make([]byte, 1<<19)
	if _, err = io.ReadFull(file, half); err != nil {
		t.Fatal(err)
	}

	changeErr := blobsChange(dir, root, change)
	rest, readErr := io.ReadAll(file)
	intact := readErr == nil && bytes.Equal(append(half, rest...), data)
	t.Logf("%-9s then %-28s  allowed: %-5v  the reader read every byte: %-5v  the name then: %s", reader, change,
		changeErr == nil, intact, blobsNameState(dir, data, other))
	if changeErr != nil {
		t.Logf("%-9s then %-28s  refused with: %v", reader, change, changeErr)
	}
}

func blobsChange(dir string, root *os.Root, change string) error {
	switch change {
	case "os.Remove":
		return os.Remove(filepath.Join(dir, "a"))
	case "Root.Remove":
		return root.Remove("a")
	case "Root.Rename over it":
		return root.Rename("b", "a")
	case "os.Rename over it":
		return os.Rename(filepath.Join(dir, "b"), filepath.Join(dir, "a"))
	}
	if err := root.Link("a", "c"); err != nil {
		return err
	}
	if err := root.Remove("a"); err != nil {
		return err
	}
	linked, err := os.ReadFile(filepath.Join(dir, "c"))
	if err == nil && !bytes.Equal(linked, blobsBytes(10, 1<<20)) {
		return io.ErrUnexpectedEOF
	}
	return err
}

// blobsNameState says what the name "a" holds after the change: the old bytes,
// the replacing ones, nothing, and whether a new file can take it
func blobsNameState(dir string, data, other []byte) string {
	held, err := os.ReadFile(filepath.Join(dir, "a"))
	switch {
	case err == nil && bytes.Equal(held, data):
		return "still the old bytes"
	case err == nil && bytes.Equal(held, other):
		return "the replacing bytes"
	case err == nil:
		return "other bytes"
	}
	file, err := os.OpenFile(filepath.Join(dir, "a"), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o600)
	if err != nil {
		return "gone, and not free: " + err.Error()
	}
	_ = file.Close()
	return "gone, and free at once"
}
