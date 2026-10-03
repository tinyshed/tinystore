package term

import (
	"os"
	"path/filepath"
	"testing"
)

func TestAFileIsNoTerminal(t *testing.T) {
	t.Setenv("FORCE_COLOR", "")
	file, err := os.Create(filepath.Join(t.TempDir(), "log"))
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	if IsTerminal(file) || Colors(file) {
		t.Fatal("a file is taken for a terminal")
	}
}

func TestNoColorAndADumbTerminalTurnColoursOff(t *testing.T) {
	t.Setenv("NO_COLOR", "1")
	if Colors(os.Stderr) {
		t.Fatal("NO_COLOR left colours on")
	}
	t.Setenv("NO_COLOR", "")
	t.Setenv("TERM", "dumb")
	if Colors(os.Stderr) {
		t.Fatal("a dumb terminal has colours")
	}
}

func TestForceColorTurnsColoursOnWithoutATerminal(t *testing.T) {
	file, err := os.Create(filepath.Join(t.TempDir(), "log"))
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	t.Setenv("NO_COLOR", "")
	t.Setenv("TERM", "")
	for force, want := range map[string]bool{"1": true, "true": true, "0": false, "": false} {
		t.Setenv("FORCE_COLOR", force)
		if got := Colors(file); got != want {
			t.Errorf("FORCE_COLOR=%q: colours %v", force, got)
		}
	}
	t.Setenv("FORCE_COLOR", "1")
	t.Setenv("NO_COLOR", "1")
	if Colors(file) {
		t.Error("FORCE_COLOR won over NO_COLOR")
	}
}
