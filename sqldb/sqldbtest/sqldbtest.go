// Package sqldbtest checks, in a program's own tests, that a database's
// migrations make what its schema declares, and writes the next migration
// when go tool tinystore asks it to. It is a package of its own so that no
// program links testing.
//
//	func TestSchema(t *testing.T) {
//		sqldbtest.CheckSchema(t, "app", AppSchema, "migrations/app")
//	}
package sqldbtest

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"sync"
	"testing"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
	"github.com/tinyshed/tinystore/sqldb"
	"github.com/tinyshed/tinystore/sqldb/internal/catalog"
)

// Request is the environment variable through which go tool tinystore asks a
// check for what it needs, as JSON: {"database", "command", "what", "answer"}.
// The command is "migrate", "new" or "schema"; the check appends its answer,
// one JSON object a line, to the file answer names.
const Request = "TINYSTORE_SQLDB"

type request struct {
	Database string `json:"database"`
	Command  string `json:"command"`
	What     string `json:"what"`
	Answer   string `json:"answer"`
}

type answer struct {
	Database    string   `json:"database"`
	Dir         string   `json:"dir"`
	Migrations  int      `json:"migrations"`
	Differences []string `json:"differences,omitempty"`
	Notes       []string `json:"notes,omitempty"`
	Wrote       string   `json:"wrote,omitempty"`
	SQL         string   `json:"sql,omitempty"`
	Unfinished  bool     `json:"unfinished,omitempty"`
	Schema      string   `json:"schema,omitempty"`
	Error       string   `json:"error,omitempty"`
}

var (
	checkedMu sync.Mutex
	checked   = map[string]bool{}
)

// CheckSchema applies the migrations in dir to a new file and compares what
// they make with schema.
//
// A difference of structure fails the test, naming each and the command that
// writes the migration. A CHECK or a default's expression spelled otherwise is
// a line to read. It never writes, unless go tool tinystore asks it to.
//
// name is the database's, as sqldb.Open takes it, and how go tool tinystore
// finds this check. A second check of one name in a test binary fails.
func CheckSchema(t testing.TB, name string, schema *sqldb.SchemaDef, dir string) {
	t.Helper()
	if !claim(name) {
		t.Fatalf("sqldbtest: %q is checked twice; one check a database", name)
	}
	asked, err := requested()
	if err != nil {
		t.Fatalf("sqldbtest: %v", err)
	}
	if asked != nil && asked.Database != "" && asked.Database != name {
		return
	}

	found, err := compare(t.Context(), t.TempDir(), name, schema, dir)
	if asked != nil {
		if err = answerTool(asked, found, schema, err); err != nil {
			t.Fatalf("sqldbtest: %v", err)
		}
		return
	}
	if err != nil {
		t.Fatalf("sql %q: %v", name, err)
	}
	report(t, name, found)
}

func claim(name string) bool {
	checkedMu.Lock()
	defer checkedMu.Unlock()
	if checked[name] {
		return false
	}
	checked[name] = true
	return true
}

func requested() (*request, error) {
	text := os.Getenv(Request)
	if text == "" {
		return nil, nil
	}
	asked := &request{}
	if err := json.Unmarshal([]byte(text), asked); err != nil {
		return nil, fmt.Errorf("%s holds %q: %w", Request, text, err)
	}
	return asked, nil
}

// comparison is what a check found: the migrations as they are, and how the
// file they make differs from the schema
type comparison struct {
	name, dir   string
	scripts     []string
	declared    *catalog.Catalog
	migrated    *catalog.Catalog
	differences []catalog.Difference
}

func compare(ctx context.Context, scratch, name string, schema *sqldb.SchemaDef, dir string) (*comparison, error) {
	abs, err := filepath.Abs(dir)
	if err != nil {
		return nil, err
	}
	found := &comparison{name: name, dir: abs}
	if found.scripts, err = fs.Glob(os.DirFS(abs), "*.sql"); err != nil {
		return nil, err
	}
	sort.Strings(found.scripts)
	if found.declared, err = catalog.Declare(ctx, schema.SQL()); err != nil {
		return nil, err
	}
	if found.migrated, err = migrate(ctx, scratch, name, abs, len(found.scripts)); err != nil {
		return found, err
	}
	found.differences = catalog.Compare(found.declared, found.migrated)
	return found, nil
}

// migrate applies the migrations to a file of its own, through sqldb.Open as
// a program's first run does, and reads what they made
func migrate(ctx context.Context, scratch, name, dir string, scripts int) (*catalog.Catalog, error) {
	if scripts == 0 {
		return &catalog.Catalog{}, nil
	}
	store, err := tinystore.Open(ctx, scratch, tinystore.Options{Manual: true})
	if err != nil {
		return nil, err
	}
	_, err = sqldb.Open(ctx, store, name, os.DirFS(dir), nil)
	if err = errors.Join(err, store.Close(ctx)); err != nil {
		return nil, fmt.Errorf("the migrations do not apply: %w", err)
	}

	var migrated *catalog.Catalog
	err = sqlite.ReadCopy(ctx, filepath.Join(scratch, "sql", name+".db"), func(r sqlite.Reader) (readErr error) {
		migrated, readErr = catalog.Read(ctx, r)
		return readErr
	})
	return migrated, err
}

// report fails the test on each difference of structure, and logs the text
// that differs:
//
//	sql "app": the schema declares what the migrations do not make:
//	  + notes.description TEXT
//	write it with: go tool tinystore migrate app new <what>
func report(t testing.TB, name string, found *comparison) {
	t.Helper()
	var lines []string
	added := true
	for _, d := range found.differences {
		if !d.Structural() {
			t.Logf("sql %q: %s", name, d.Sentence("", catalog.Migrations))
			continue
		}
		lines = append(lines, "  "+d.Line())
		added = added && strings.HasPrefix(d.Line(), "+")
	}
	if len(lines) == 0 {
		return
	}
	header := "the schema and its migrations differ:"
	if added {
		header = "the schema declares what the migrations do not make:"
	}
	t.Errorf("sql %q: %s\n%s\nwrite it with: go tool tinystore migrate %s new <what>",
		name, header, strings.Join(lines, "\n"), name)
}

// answerTool writes what go tool tinystore asked for into its answer file
func answerTool(asked *request, found *comparison, schema *sqldb.SchemaDef, failed error) error {
	given := answer{Schema: schema.SQL()}
	if found != nil {
		given.Database, given.Dir, given.Migrations = found.name, found.dir, len(found.scripts)
	}
	switch {
	case failed != nil:
		given.Error = failed.Error()
	case asked.Command == "new":
		given.Wrote, given.SQL, given.Unfinished, failed = found.writeNext(asked.What)
	}
	if found != nil && found.migrated != nil {
		for _, d := range found.differences {
			if d.Structural() {
				given.Differences = append(given.Differences, d.Line())
			} else {
				given.Notes = append(given.Notes, d.Sentence("", catalog.Migrations))
			}
		}
	}
	if failed != nil {
		given.Error = failed.Error()
	}
	return appendAnswer(asked.Answer, given)
}

var validWhat = regexp.MustCompile(`^[a-z0-9][a-z0-9_]*$`)

// writeNext writes the migration that makes the file what the schema declares,
// named after the last one: 001_notes.sql → 002_<what>.sql
func (c *comparison) writeNext(what string) (path, text string, unfinished bool, err error) {
	if !validWhat.MatchString(what) {
		return "", "", false, fmt.Errorf("a migration's name is lower case letters, digits and _, not %q", what)
	}
	text, unfinished = catalog.Draft(c.declared, c.migrated, c.differences)
	if text == "" {
		return "", "", false, nil
	}
	path = filepath.Join(c.dir, c.nextName(what))
	if err = os.WriteFile(path, []byte(text), 0o600); err != nil {
		return "", "", false, err
	}
	return path, text, unfinished, nil
}

var numbered = regexp.MustCompile(`^[0-9]+`)

// nextName numbers a migration one past the last, as wide as the others are
func (c *comparison) nextName(what string) string {
	last, width := 0, 3
	for _, script := range c.scripts {
		digits := numbered.FindString(script)
		if n, err := strconv.Atoi(digits); err == nil {
			last, width = max(last, n), len(digits)
		}
	}
	return fmt.Sprintf("%0*d_%s.sql", width, last+1, what)
}

func appendAnswer(path string, given answer) (err error) {
	line, err := json.Marshal(given)
	if err != nil {
		return err
	}
	appending := os.O_CREATE | os.O_APPEND | os.O_WRONLY
	file, err := os.OpenFile(path, appending, 0o600) //nolint:gosec // the tool names its own file
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, file.Close()) }()
	_, err = file.Write(append(line, '\n'))
	return err
}
