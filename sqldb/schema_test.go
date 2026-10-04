package sqldb

import (
	"os"
	"strings"
	"testing"
	"testing/fstest"
	"time"
	"uuid"
)

// the model research's design/sqldb.md declares
type (
	User struct {
		ID        int64 `db:",generated"`
		Email     string
		Name      string `db:"display_name"`
		CreatedAt time.Time
		Password  string `db:"-"`
	}

	Note struct {
		ID        uuid.UUID
		AuthorID  int64
		Title     string
		Done      bool
		Tags      JSON[[]string]
		Due       *Date
		CreatedAt time.Time
	}
)

// design is the schema of research's design/sqldb.md, declared as a program declares it
type design struct {
	users  *TableDef[User]
	notes  *TableDef[Note]
	schema *SchemaDef
}

func declareDesign(t *testing.T) design {
	t.Helper()
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

// the SQL a schema prints is research's design/sqldb.md's, byte for byte
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
