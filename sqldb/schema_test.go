package sqldb

import (
	"os"
	"reflect"
	"strings"
	"testing"
	"testing/fstest"
	"time"
)

// UUID stands for github.com/google/uuid's, which the module does not
// require: knowUUIDs makes this package one whose UUID sqldb knows by name
type UUID [16]byte

// knowUUIDs lets UUID stand for a known uuid until the test ends, or until
// the function it returns is called
func knowUUIDs(t *testing.T) (forget func()) {
	t.Helper()
	path := reflect.TypeFor[UUID]().PkgPath()
	uuidPackages[path] = true
	forgetTypes()
	forget = func() {
		delete(uuidPackages, path)
		forgetTypes()
	}
	t.Cleanup(forget)
	return forget
}

// forgetTypes drops what sqldb learnt of Go types, so that a change of
// uuidPackages shows
func forgetTypes() {
	classified.Clear()
	models.Clear()
	plans.Clear()
}

// the model docs/sqldb.md declares
type (
	User struct {
		ID        int64 `db:",generated"`
		Email     string
		Name      string `db:"display_name"`
		CreatedAt time.Time
		Password  string `db:"-"`
	}

	Note struct {
		ID        UUID
		AuthorID  int64
		Title     string
		Done      bool
		Tags      JSON[[]string]
		Due       *Date
		CreatedAt time.Time
	}
)

// design is docs/sqldb.md's schema, declared as a program declares it
type design struct {
	users  *TableDef[User]
	notes  *TableDef[Note]
	schema *SchemaDef
}

func declareDesign(t *testing.T) design {
	t.Helper()
	knowUUIDs(t)
	users := Table[User]("users",
		PrimaryKey("id"),
		Unique("email"),
	)
	notes := Table[Note]("notes",
		PrimaryKey("id"),
		References("author_id", users, Cascade),
		Default("done", false),
		Default("tags", []string{}),
		Index("author_id", "created_at"),
		NamedIndex("notes_open", "done", "due"),
		Check("length(title) > 0"),
	)
	return design{users: users, notes: notes, schema: Schema(users, notes)}
}

// designMigrations make the design's schema as its SQL prints it
func designMigrations(t *testing.T) fstest.MapFS {
	t.Helper()
	golden, err := os.ReadFile("testdata/schema.sql")
	if err != nil {
		t.Fatal(err)
	}
	return fstest.MapFS{"001_notes.sql": {Data: golden}}
}

// the SQL a schema prints is docs/sqldb.md's, byte for byte
func TestASchemaIsTheSQLItPrints(t *testing.T) {
	golden, err := os.ReadFile("testdata/schema.sql")
	if err != nil {
		t.Fatal(err)
	}
	want := strings.ReplaceAll(string(golden), "\r\n", "\n")
	if got := declareDesign(t).schema.SQL(); got != want {
		t.Fatalf("the schema prints\n%s\nwant\n%s", got, want)
	}
}

type keyworded struct {
	ID    int64 `db:",generated"`
	Order int
	Group string `db:"the group"`
}

// a name SQL reads as a keyword, or one it cannot read bare, is quoted
func TestANameSQLWouldMisreadIsQuoted(t *testing.T) {
	table := Table[keyworded]("order", PrimaryKey("id"), Index("order"))
	want := "CREATE TABLE \"order\" (\n" +
		"    id          INTEGER PRIMARY KEY,\n" +
		"    \"order\"     INTEGER NOT NULL,\n" +
		"    \"the group\" TEXT NOT NULL\n" +
		") STRICT;\n" +
		"CREATE INDEX order_order ON \"order\" (\"order\");\n"
	if got := Schema(table).SQL(); got != want {
		t.Fatalf("printed\n%s\nwant\n%s", got, want)
	}
	if table.insert != `INSERT INTO "order" ("order", "the group") VALUES (?, ?)` {
		t.Fatalf("inserts with %s", table.insert)
	}
}

func TestSchemaNamesANilTable(t *testing.T) {
	if got := panicOf(func() { Schema(nil) }); !strings.Contains(got, "a nil table") {
		t.Fatalf("a nil table panicked with %q", got)
	}
}
