package main

import (
	"bytes"
	"database/sql"
	"path/filepath"
	"testing"
)

func TestTheExampleRunsTwice(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "data")
	for _, want := range []string{
		"note 1: groceries — milk, bread\ndraft of note 1: milk, bread, eggs, tea\n" +
			"attachment list.txt: 22 bytes, starts \"milk\"; note 1 holds 22\n" +
			"notes indexed: 2, reminders ahead: 1\nnotes created: 2\nrequests timed: 2\nlog lines: 2\n" +
			"backup written\n",
		"note 1: groceries — milk, bread\nnote 2: groceries — milk, bread\ndraft of note 1: milk, bread, eggs, tea\n" +
			"attachment list.txt: 22 bytes, starts \"milk\"; note 1 holds 22\n" +
			"notes indexed: 2, reminders ahead: 2\nnotes created: 2\nrequests timed: 2\nlog lines: 4\n" +
			"backup written\n",
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

// a program using every engine registers no SQL driver by name, so it may
// link one of its own, mattn/go-sqlite3 among them, which takes "sqlite3"
func TestTheExampleRegistersNoSQLDriver(t *testing.T) {
	if names := sql.Drivers(); len(names) > 0 {
		t.Fatalf("linking the store registered %v with database/sql", names)
	}
}
