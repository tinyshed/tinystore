package sqldbtest

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/sqldb"
	_ "github.com/tinyshed/tinystore/sqldb/fts5"
	_ "github.com/tinyshed/tinystore/sqldb/rtree"
)

// recorder is a testing.TB that keeps what a check reports; its Fatalf stops
// the check, as the real one stops a test
type recorder struct {
	testing.TB
	errors []string
	logs   []string
}

type stopped struct{}

func (r *recorder) Helper() {}

func (r *recorder) Errorf(format string, args ...any) {
	r.errors = append(r.errors, fmt.Sprintf(format, args...))
}

func (r *recorder) Logf(format string, args ...any) {
	r.logs = append(r.logs, fmt.Sprintf(format, args...))
}

func (r *recorder) Fatalf(format string, args ...any) {
	r.Errorf(format, args...)
	panic(stopped{})
}

// check runs one CheckSchema as a fresh test binary would
func check(t *testing.T, schema *sqldb.SchemaDef, dir string) *recorder {
	t.Helper()
	checked = map[string]bool{}
	r := &recorder{TB: t}
	func() {
		defer func() {
			if p := recover(); p != nil {
				if _, ok := p.(stopped); !ok {
					panic(p)
				}
			}
		}()
		CheckSchema(r, "app", schema, dir)
	}()
	return r
}

// ask runs one CheckSchema as go tool tinystore does, and reads its answer
func ask(t *testing.T, schema *sqldb.SchemaDef, dir, command, what string) answer {
	t.Helper()
	path := filepath.Join(t.TempDir(), "answer.jsonl")
	asked, err := json.Marshal(request{Database: "app", Command: command, What: what, Answer: path})
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv(Request, string(asked))
	if r := check(t, schema, dir); len(r.errors) > 0 {
		t.Fatalf("the tool's check failed: %v", r.errors)
	}
	text, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var given answer
	if err = json.Unmarshal(text, &given); err != nil {
		t.Fatalf("%v in %s", err, text)
	}
	return given
}

func migrations(t *testing.T, files map[string]string) string {
	t.Helper()
	dir := t.TempDir()
	for name, text := range files {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(text), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	return dir
}

func listed(t *testing.T, dir string) string {
	t.Helper()
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for _, entry := range entries {
		names = append(names, entry.Name())
	}
	return strings.Join(names, " ")
}

type (
	Note struct {
		ID        int64 `db:",generated"`
		Title     string
		CreatedAt time.Time
	}
	Described struct {
		Note
		Description string
	}
	Renamed struct {
		ID        int64 `db:",generated"`
		Text      string
		CreatedAt time.Time
	}
	Counted struct {
		ID     int64 `db:",generated"`
		Status int64
	}
)

const firstNotes = `CREATE TABLE notes (
    id         INTEGER PRIMARY KEY,
    title      TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    CHECK ( length(title)>0 )
) STRICT;
`

// A check fails on what the schema declares and the migrations do not make,
// names the command that writes it, and writes nothing. The tool asks it to
// write the next migration, after which the check passes.
func TestCheckSchemaFindsWhatIsMissingAndWritesOnlyWhenAsked(t *testing.T) {
	notes := sqldb.Table[Note]("notes", sqldb.PrimaryKey("id"), sqldb.Check("length(title) > 0"))
	dir := migrations(t, map[string]string{"001_notes.sql": firstNotes})
	if r := check(t, sqldb.Schema(notes), dir); len(r.errors) > 0 || len(r.logs) > 0 {
		t.Fatalf("migrations making the schema: %v, %v", r.errors, r.logs)
	}

	later := sqldb.Schema(sqldb.Table[Described]("notes", sqldb.PrimaryKey("id"), sqldb.Default("description", ""),
		sqldb.Index("created_at"), sqldb.Check("length(title) > 1")))
	r := check(t, later, dir)
	want := `sql "app": the schema declares what the migrations do not make:` + "\n" +
		`  + notes.description TEXT NOT NULL DEFAULT ''` + "\n" +
		`  + INDEX notes_created_at ON notes (created_at)` + "\n" +
		`write it with: go tool tinystore migrate app new <what>`
	if len(r.errors) != 1 || r.errors[0] != want || listed(t, dir) != "001_notes.sql" {
		t.Fatalf("a schema ahead of its migrations failed with\n%s\nwant\n%s\nand left %s", r.errors, want, listed(t, dir))
	}
	if len(r.logs) != 2 || !strings.Contains(r.logs[0], "notes checks length(title) > 1 in the schema") {
		t.Fatalf("the check spelled otherwise was logged as %q", r.logs)
	}

	reported := ask(t, later, dir, "migrate", "")
	if reported.Migrations != 1 || len(reported.Differences) != 2 || reported.Wrote != "" || listed(t, dir) != "001_notes.sql" {
		t.Fatalf("the tool's report: %+v", reported)
	}
	wrote := ask(t, later, dir, "new", "add_description")
	if filepath.Base(wrote.Wrote) != "002_add_description.sql" || wrote.Unfinished ||
		wrote.SQL != "ALTER TABLE notes ADD COLUMN description TEXT NOT NULL DEFAULT '';\n"+
			"CREATE INDEX notes_created_at ON notes (created_at);\n" {
		t.Fatalf("the tool wrote %+v", wrote)
	}
	if schema := ask(t, later, dir, "schema", ""); schema.Schema != later.SQL() {
		t.Fatalf("the tool's schema: %q", schema.Schema)
	}
	t.Setenv(Request, "")
	if r = check(t, later, dir); len(r.errors) > 0 {
		t.Fatalf("after the migration was written: %v", r.errors)
	}
}

// a change the tool cannot decide, a rename or a new type, is a draft whose
// TODO keeps it from running until a person writes what it asks
func TestAnAmbiguousChangeIsADraftThatDoesNotRun(t *testing.T) {
	notes := sqldb.Schema(sqldb.Table[Note]("notes", sqldb.PrimaryKey("id"), sqldb.Check("length(title) > 0")))
	renamed := sqldb.Schema(sqldb.Table[Renamed]("notes", sqldb.PrimaryKey("id"), sqldb.Check("length(text) > 0")))
	dir := migrations(t, map[string]string{"001_notes.sql": firstNotes})
	if r := check(t, notes, dir); len(r.errors) > 0 {
		t.Fatal(r.errors)
	}

	draft := ask(t, renamed, dir, "new", "rename_title")
	if !draft.Unfinished || !strings.Contains(draft.SQL, "-- REVIEW: notes loses title and gains text") ||
		!strings.Contains(draft.SQL, "ALTER TABLE notes /* TODO: RENAME COLUMN title TO …") {
		t.Fatalf("a rename or a replacement drafted as\n%s", draft.SQL)
	}
	t.Setenv(Request, "")
	if r := check(t, renamed, dir); len(r.errors) != 1 || !strings.Contains(r.errors[0], "the migrations do not apply") {
		t.Fatalf("an unfinished draft ran: %v", r.errors)
	}
	finished := "ALTER TABLE notes RENAME COLUMN title TO text;\n"
	if err := os.WriteFile(draft.Wrote, []byte(finished), 0o600); err != nil {
		t.Fatal(err)
	}
	if r := check(t, renamed, dir); len(r.errors) > 0 {
		t.Fatalf("the finished draft: %v", r.errors)
	}

	counted := sqldb.Schema(sqldb.Table[Counted]("counts", sqldb.PrimaryKey("id")))
	counts := migrations(t, map[string]string{"001_counts.sql": `CREATE TABLE counts (
		id INTEGER PRIMARY KEY, status TEXT NOT NULL) STRICT;`})
	rebuilt := ask(t, counted, counts, "new", "status_number")
	for _, want := range []string{
		"-- REVIEW: SQLite cannot make this change in place; counts is rebuilt:",
		"--   counts.status is INTEGER in the schema and TEXT in the migrations",
		"CREATE TABLE counts_new (",
		"INSERT INTO counts_new (id, status) SELECT id, /* TODO: how does each old status, TEXT, become INTEGER? */ FROM counts;",
		"DROP TABLE counts;\nALTER TABLE counts_new RENAME TO counts;\n",
	} {
		if !rebuilt.Unfinished || !strings.Contains(rebuilt.SQL, want) {
			t.Fatalf("a new type drafted as\n%s\nwant %q", rebuilt.SQL, want)
		}
	}
	t.Setenv(Request, "")
	if r := check(t, counted, counts); len(r.errors) != 1 || !strings.Contains(r.errors[0], "the migrations do not apply") {
		t.Fatalf("an unfinished rebuild ran: %v", r.errors)
	}
}

func TestTwoChecksOfOneNameFail(t *testing.T) {
	notes := sqldb.Schema(sqldb.Table[Note]("notes", sqldb.PrimaryKey("id"), sqldb.Check("length(title) > 0")))
	dir := migrations(t, map[string]string{"001_notes.sql": firstNotes})
	check(t, notes, dir)
	r := &recorder{TB: t}
	func() {
		defer func() { _ = recover() }()
		CheckSchema(r, "app", notes, dir)
	}()
	if len(r.errors) != 1 || !strings.Contains(r.errors[0], `"app" is checked twice`) {
		t.Fatalf("a second check of app: %v", r.errors)
	}
}

// the shadow tables a virtual table keeps are SQLite's, which no schema
// declares and the check does not count as the application's
func TestAVirtualTablesShadowsAreNotTheSchemas(t *testing.T) {
	notes := sqldb.Table[Note]("notes", sqldb.PrimaryKey("id"), sqldb.Check("length(title) > 0"))
	dir := migrations(t, map[string]string{
		"001_notes.sql": firstNotes,
		"002_search.sql": "create virtual table notes_fts using fts5(title, content = 'notes', content_rowid = 'id');\n" +
			"create virtual table places using rtree(id, min_x, max_x);\n",
	})
	if r := check(t, sqldb.Schema(notes), dir); len(r.errors) > 0 || len(r.logs) > 0 {
		t.Fatalf("a virtual table's shadows were taken for the application's tables: %v, %v", r.errors, r.logs)
	}
}
