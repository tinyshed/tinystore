package main

import (
	"bytes"
	"path/filepath"
	"testing"
)

func TestTheExampleRunsTwice(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "data")
	for _, want := range []string{
		"note 1: groceries — milk, bread\nnotes created: 2\nlog lines: 2\nbackup written\n",
		"note 1: groceries — milk, bread\nnote 2: groceries — milk, bread\nnotes created: 2\nlog lines: 4\nbackup written\n",
	} {
		var out bytes.Buffer
		if err := run(t.Context(), dir, &out); err != nil {
			t.Fatalf("%v\n%s", err, out.String())
		}
		if out.String() != want {
			t.Fatalf("printed:\n%s\nwant:\n%s", out.String(), want)
		}
	}
}
