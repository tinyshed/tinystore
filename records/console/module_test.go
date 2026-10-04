package console

import (
	"os/exec"
	"strings"
	"testing"
)

// a program logging to the console alone links neither the store nor SQLite nor zstd
func TestTheConsoleImportsNoStore(t *testing.T) {
	out, err := exec.Command("go", "list", "-deps", ".").Output()
	if err != nil {
		t.Fatal(err)
	}
	for _, path := range strings.Fields(string(out)) {
		switch {
		case strings.Contains(path, "ncruces"), strings.Contains(path, "klauspost"), path == "database/sql",
			path == "github.com/tinyshed/tinystore", path == "github.com/tinyshed/tinystore/records":
			t.Errorf("records/console links %s", path)
		}
	}
}
