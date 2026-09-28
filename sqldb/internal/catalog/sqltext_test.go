package catalog

import (
	"context"
	"fmt"
	"reflect"
	"strings"
	"testing"
)

func TestRebuildRestoresTriggers(t *testing.T) {
	ctx := context.Background()
	oldSQL := `CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT NOT NULL) STRICT;
		CREATE INDEX notes_lower_title ON notes (lower(title));
		CREATE TRIGGER notes_audit AFTER UPDATE ON notes BEGIN SELECT new.id; END;`
	have, err := Declare(ctx, oldSQL)
	if err != nil {
		t.Fatal(err)
	}
	want, err := Declare(ctx, `CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT) STRICT;`)
	if err != nil {
		t.Fatal(err)
	}
	draft, _ := Draft(want, have, Compare(want, have))
	if !strings.Contains(draft, "CREATE TRIGGER notes_audit AFTER UPDATE ON notes BEGIN SELECT new.id; END;") {
		t.Fatalf("rebuild dropped the trigger:\n%s", draft)
	}
	rebuilt, err := Declare(ctx, oldSQL+draft)
	if err != nil || len(rebuilt.Table("notes").Triggers) != 1 || rebuilt.Table("notes").index("notes_lower_title") == nil {
		t.Fatalf("a rebuilt table lost an application index or trigger: %v, %+v", err, rebuilt)
	}
}

// two spellings of one rule fold to one string, and a quoted text keeps its
// case and its spaces
func TestFoldMakesTwoSpellingsOfOneRuleOne(t *testing.T) {
	for _, pair := range [][2]string{
		{"CHECK ( done IN(0,1) )", "check (done in (0, 1))"},
		{`"Done" IN (0, 1) -- a bool`, "done in (0,1)"},
		{"json_valid ( tags )", "JSON_VALID(tags)"},
		{"/* why */ due IS date(due)", "due is date( due )"},
	} {
		if Fold(pair[0]) != Fold(pair[1]) {
			t.Errorf("%q folds to %q, %q to %q", pair[0], Fold(pair[0]), pair[1], Fold(pair[1]))
		}
	}
	if Fold("status = 'Open Now'") == Fold("status = 'open now'") {
		t.Error("a quoted text lost its case")
	}
}

// a default's text is a literal only when SQLite would compute it without a
// function or a name, and its value is what SQLite would compute
func TestALiteralIsTheValueSQLiteComputes(t *testing.T) {
	for text, want := range map[string]any{
		"0": int64(0), "00": int64(0), "-1": int64(-1), "+2": int64(2), "1.5": 1.5, "-1.5": -1.5,
		"1e3": 1000.0, "0x10": int64(16), "'it''s'": "it's", "''": "", "X'0102'": []byte{1, 2}, "NULL": nil,
		"TRUE": int64(1), "false": int64(0), "9223372036854775808": 9223372036854775808.0,
	} {
		got, isLiteral := literal(text)
		if !isLiteral || !reflect.DeepEqual(got, want) {
			t.Errorf("%s is %#v, %t; want %#v", text, got, isLiteral, want)
		}
	}
	for _, text := range []string{"lower(hex(randomblob(4)))", "CURRENT_TIMESTAMP", "1 + 1", "-x", "'a' || 'b'", ""} {
		if value, isLiteral := literal(text); isLiteral {
			t.Errorf("%q is a literal of %#v", text, value)
		}
	}
}

// a CREATE TABLE splits into its columns and constraints at the commas of its
// own parentheses, each with the checks it holds
func TestElementsSplitACreateTable(t *testing.T) {
	create := `CREATE TABLE "my notes" (
		id INTEGER PRIMARY KEY,
		"Title" TEXT NOT NULL DEFAULT 'a, b' CHECK (length("Title") > 0),
		tags TEXT CHECK (json_valid(tags) AND json_type(tags) IN ('array', 'object')),
		CONSTRAINT titled CHECK (Title <> '(')
	) STRICT`
	var got []string
	for _, e := range elements(create) {
		got = append(got, fmt.Sprintf("%s|%s", e.name, strings.Join(e.checks, ";")))
	}
	want := []string{
		"id|",
		`Title|length("Title") > 0`,
		"tags|json_valid(tags) AND json_type(tags) IN ('array', 'object')",
		"|Title <> '('",
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("elements\n%q\nwant\n%q", got, want)
	}
}

func TestQuoteNamesSQLWouldMisread(t *testing.T) {
	for name, want := range map[string]string{
		"notes": "notes", "created_at": "created_at", "order": `"order"`, "Key": `"Key"`,
		"the group": `"the group"`, "2fa": `"2fa"`, `say "hi"`: `"say ""hi"""`,
	} {
		if got := Quote(name); got != want {
			t.Errorf("%s → %s, want %s", name, got, want)
		}
	}
}

// a rebuild's new table is the declared one under another name, whatever the
// name was spelled as
func TestRenamedReplacesOnlyTheTablesName(t *testing.T) {
	for create, want := range map[string]string{
		"CREATE TABLE notes (\n    id INTEGER\n) STRICT": "CREATE TABLE notes_new (\n    id INTEGER\n) STRICT",
		`CREATE TABLE "notes" (notes TEXT)`:              `CREATE TABLE notes_new (notes TEXT)`,
	} {
		if got := renamed(create, "notes_new"); got != want {
			t.Errorf("%q → %q, want %q", create, got, want)
		}
	}
}
