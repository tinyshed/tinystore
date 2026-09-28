package main

import (
	"bytes"
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestTheToolDoesNotTrustAnAnswerFromAFailedTest(t *testing.T) {
	root := t.TempDir()
	write(t, root, map[string]string{
		"go.mod": "module example.com/failing\n\ngo 1.27.0\n",
		"fixture/schema_test.go": `package fixture
import (
    "encoding/json"
    "os"
    "testing"
)
func TestSchema(t *testing.T) {
    var request struct { Answer string ` + "`json:\"answer\"`" + ` }
    if err := json.Unmarshal([]byte(os.Getenv("TINYSTORE_SQLDB")), &request); err != nil { t.Fatal(err) }
    if err := os.WriteFile(request.Answer, []byte("{\"database\":\"app\"}\n"), 0600); err != nil { t.Fatal(err) }
    t.Fatal("another check failed")
}`,
	})
	_, err := ask(context.Background(), root,
		check{database: "app", test: "TestSchema", dir: filepath.Join(root, "fixture")}, command{verb: "schema"})
	if err == nil || !strings.Contains(err.Error(), "another check failed") {
		t.Fatalf("the tool accepted a failed test's answer: %v", err)
	}
}

func write(t *testing.T, root string, files map[string]string) {
	t.Helper()
	for name, text := range files {
		path := filepath.Join(root, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(path), 0o750); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(text), 0o600); err != nil {
			t.Fatal(err)
		}
	}
}

const checksOfTwo = `package data

import (
	"testing"

	check "github.com/tinyshed/tinystore/sqldb/sqldbtest"
)

func TestSchema(t *testing.T) {
	check.CheckSchema(t, "app", AppSchema, "migrations/app")
	check.CheckSchema(t, "billing", BillingSchema, "migrations/billing")
	check.CheckSchema(t, name(), AppSchema, "migrations/app")
}

func checkAudit(t *testing.T) {
	check.CheckSchema(t, "audit", AuditSchema, "migrations/audit")
}
`

// the tool finds each check by its database's name, in the test function that
// calls it, across the module and not in its nested modules or testdata
func TestTheToolFindsEachDatabaseByItsName(t *testing.T) {
	root := t.TempDir()
	write(t, root, map[string]string{
		"go.mod":                           "module example.com/app\n",
		"internal/data/schema_test.go":     checksOfTwo,
		"internal/data/testdata/x_test.go": strings.ReplaceAll(checksOfTwo, `"app"`, `"fixture"`),
		"tools/go.mod":                     "module example.com/app/tools\n",
		"tools/x_test.go":                  strings.ReplaceAll(checksOfTwo, `"app"`, `"nested"`),
		"other/other_test.go":              "package other\n\nfunc TestNothing() {}\n",
	})
	checks, unnamed, err := findChecks(root)
	if err != nil {
		t.Fatal(err)
	}
	var found []string
	for _, c := range checks {
		found = append(found, c.database+" in "+c.test+" of "+filepath.Base(c.dir))
	}
	if strings.Join(found, "; ") != "app in TestSchema of data; billing in TestSchema of data; audit in  of data" ||
		len(unnamed) != 1 {
		t.Fatalf("found %q and %d unnamed", found, len(unnamed))
	}

	if chosen, pickErr := pick(checks, unnamed, "billing", false); pickErr != nil || chosen[0].database != "billing" {
		t.Fatalf("billing: %v, %v", chosen, pickErr)
	}
	if _, err = pick(checks, unnamed, "", false); err == nil || !strings.Contains(err.Error(), "name one") {
		t.Fatalf("a command for one database, with none named: %v", err)
	}
	if _, err = pick(checks, unnamed, "nested", false); err == nil || !strings.Contains(err.Error(), `no test checks "nested"`) {
		t.Fatalf("a database of a nested module: %v", err)
	}
	if _, err = ask(t.Context(), root, checks[2], command{verb: "migrate"}); err == nil ||
		!strings.Contains(err.Error(), "is in a helper") {
		t.Fatalf("a check in a helper: %v", err)
	}
	twice := append(checks, check{database: "app", test: "TestAgain", at: "elsewhere_test.go:3"})
	if _, err = pick(twice, nil, "app", false); err == nil || !strings.Contains(err.Error(), `"app" is checked twice`) {
		t.Fatalf("a database checked twice: %v", err)
	}
}

func TestTheArgumentsAreACommand(t *testing.T) {
	for line, want := range map[string]command{
		"migrate":               {verb: "migrate"},
		"migrate app":           {verb: "migrate", database: "app"},
		"migrate new add_x":     {verb: "new", what: "add_x"},
		"migrate app new add_x": {verb: "new", database: "app", what: "add_x"},
		"schema":                {verb: "schema"},
		"schema billing":        {verb: "schema", database: "billing"},
	} {
		if got, err := parse(strings.Fields(line)); err != nil || got != want {
			t.Errorf("%s → %+v, %v", line, got, err)
		}
	}
	for _, line := range []string{"", "migrate app new", "schema a b", "serve"} {
		if _, err := parse(strings.Fields(line)); err == nil {
			t.Errorf("%q parsed", line)
		}
	}
}

const appSchema = `package data

import (
	"time"

	"github.com/tinyshed/tinystore/sqldb"
)

type Note struct {
	ID          int64 ` + "`db:\",generated\"`" + `
	Title       string
	Description string
	CreatedAt   time.Time
}

var AppSchema = sqldb.Schema(sqldb.Table[Note]("notes", sqldb.PrimaryKey("id"), sqldb.Default("description", "")))
`

const appCheck = `package data

import (
	"testing"

	"github.com/tinyshed/tinystore/sqldb/sqldbtest"
)

func TestSchema(t *testing.T) {
	sqldbtest.CheckSchema(t, "app", AppSchema, "migrations/app")
}
`

// the tool runs the check of the database it names, which writes the next
// migration, and says what it wrote; after it the check passes
func TestTheToolWritesTheNextMigrationThroughTheCheck(t *testing.T) {
	if testing.Short() {
		t.Skip("builds a module against the library")
	}
	repo, err := filepath.Abs("../..")
	if err != nil {
		t.Fatal(err)
	}
	sums, err := os.ReadFile(filepath.Join(repo, "go.sum"))
	if err != nil {
		t.Fatal(err)
	}
	root := t.TempDir()
	write(t, root, map[string]string{
		"go.mod": "module example.com/app\n\ngo 1.27.0\n\nrequire github.com/tinyshed/tinystore v0.0.0\n\n" +
			"replace github.com/tinyshed/tinystore => " + filepath.ToSlash(repo) + "\n",
		"go.sum":                       string(sums),
		"internal/data/schema.go":      appSchema,
		"internal/data/schema_test.go": appCheck,
		"internal/data/migrations/app/001_notes.sql": "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT NOT NULL, " +
			"created_at INTEGER NOT NULL) STRICT;\n",
	})
	for name, value := range map[string]string{"GOFLAGS": "-mod=mod", "GOPROXY": "off", "GOSUMDB": "off", "GOWORK": "off"} {
		t.Setenv(name, value)
	}
	t.Chdir(root)

	var out bytes.Buffer
	if err = run(t.Context(), []string{"migrate"}, &out); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), "app: 1 migration, and the schema differs:\n  + notes.description TEXT NOT NULL DEFAULT ''") {
		t.Fatalf("migrate printed\n%s", out.String())
	}
	out.Reset()
	if err = run(t.Context(), []string{"migrate", "new", "add_description"}, &out); err != nil {
		t.Fatal(err)
	}
	want := "wrote internal/data/migrations/app/002_add_description.sql:\n\n" +
		"    ALTER TABLE notes ADD COLUMN description TEXT NOT NULL DEFAULT '';\n\n"
	if out.String() != want {
		t.Fatalf("migrate new printed\n%q\nwant\n%q", out.String(), want)
	}
	out.Reset()
	if err = run(t.Context(), []string{"migrate", "app"}, &out); err != nil ||
		out.String() != "app: 2 migrations, and the schema matches them\n" {
		t.Fatalf("after the migration: %q, %v", out.String(), err)
	}
	out.Reset()
	if err = run(t.Context(), []string{"schema"}, &out); err != nil || !strings.HasPrefix(out.String(), "CREATE TABLE notes (") {
		t.Fatalf("schema printed %q, %v", out.String(), err)
	}
}
