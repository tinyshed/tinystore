package sqldb

import (
	"database/sql/driver"
	"fmt"
	"reflect"
	"strings"
	"testing"
	"time"
)

type (
	orphan struct {
		ID     int64 `db:",generated"`
		Parent int64
		Label  string `db:",generated"`
	}
	withMap struct {
		ID    int64
		Attrs map[string]int
	}
	withCents struct {
		ID    int64
		Price Cents
	}
	keyedTwice struct {
		A, B int64
	}
	child struct {
		ID       int64 `db:",generated"`
		ParentA  int64
		ParentID string
		Maybe    *int64
	}
	oddTag struct {
		ID int64 `db:",autoincrement"`
	}
	generatedEmbed struct {
		Inner `db:",generated"`
	}
	shadowed struct {
		Inner
		Other Inner `db:"-"`
		Title string
	}
	Inner struct {
		Title string
	}
	twice struct {
		A string `db:"name"`
		B string `db:"name"`
	}
)

type scanOnly int64

func (*scanOnly) Scan(any) error { return nil }

type valueOnly int64

func (valueOnly) Value() (driver.Value, error) { return int64(0), nil }

func TestDeclarationsRequireBothCustomConversions(t *testing.T) {
	for _, declare := range []func(){
		func() { Table[struct{ Value scanOnly }]("scan_only", Storage("value", Integer)) },
		func() { Table[struct{ Value valueOnly }]("value_only", Storage("value", Integer)) },
	} {
		if got := panicOf(declare); !strings.Contains(got, "both sql.Scanner and driver.Valuer") {
			t.Errorf("incomplete custom type: %s", got)
		}
	}
}

func TestAnIndexOptionCanBeUsedForSeveralTables(t *testing.T) {
	option := Index("a")
	first := Table[keyedTwice]("first", option)
	second := Table[keyedTwice]("second", option)
	if first.table.indexes[0].name != "first_a" || second.table.indexes[0].name != "second_a" {
		t.Fatalf("index names: %s, %s", first.table.indexes[0].name, second.table.indexes[0].name)
	}
}

func TestAnUnknownDeleteActionFailsAtDeclaration(t *testing.T) {
	parent := Table[User]("parents", PrimaryKey("id"))
	got := panicOf(func() { Table[child]("children", References("parent_a", parent, Action(99))) })
	if !strings.Contains(got, "unknown action on delete 99") {
		t.Fatalf("unknown action: %s", got)
	}
}

// a declaration that cannot be a table panics when the program starts, naming
// the table and what cannot be
func TestADeclarationThatCannotBeATableFailsAtStart(t *testing.T) {
	users := Table[User]("users", PrimaryKey("id"))
	pairs := Table[keyedTwice]("pairs", PrimaryKey("a", "b"))
	for want, declare := range map[string]func(){
		"table notes: notes has no column nope": func() { Table[orphan]("notes", Index("nope")) },
		"the default of label: a bool, and the field is string": func() {
			Table[orphan]("notes", PrimaryKey("id"), Default("label", true))
		},
		"notes.label is generated, and nothing fills it": func() { Table[orphan]("notes", PrimaryKey("id")) },
		"sqldb does not store map[string]int; sqldb.JSON[map[string]int] keeps it as JSON": func() {
			Table[withMap]("maps")
		},
		`prices.price: sqldb does not know how sqldb.Cents is stored; say sqldb.Storage("price", sqldb.Text)`: func() {
			Table[withCents]("prices")
		},
		"parent_a references pairs, whose primary key is (a, b); this version refers to a key of one column": func() {
			Table[child]("children", References("parent_a", pairs))
		},
		"parent_id is TEXT and users.id INTEGER": func() { Table[child]("children", References("parent_id", users)) },
		"parent_a is set to NULL on delete, and int64 cannot be NULL": func() {
			Table[child]("children", References("parent_a", users, SetNull))
		},
		"two primary keys": func() { Table[orphan]("notes", PrimaryKey("id"), PrimaryKey("parent")) },
		"two indexes named pairs_x": func() {
			Table[keyedTwice]("pairs", NamedIndex("pairs_x", "a"), NamedIndex("pairs_x", "b"))
		},
		`the db tag ",autoincrement" has an option sqldb does not know`: func() { Table[oddTag]("odd") },
		"generated applies to a column, not an embedded struct":         func() { Table[generatedEmbed]("embedded") },
		"done is a bool, which sqldb stores as INTEGER; Storage is for a type it does not know, or a uuid": func() {
			Table[Note]("notes", Storage("done", Text))
		},
		"sqlite_notes is a name SQLite or sqldb keeps for itself": func() { Table[User]("sqlite_notes") },
		"twice.A and twice.B are both the column name":            func() { Table[twice]("twice") },
		"schema: two tables named USERS":                          func() { Schema(users, Table[User]("USERS", PrimaryKey("id"))) },
		"children.parent_a references users, which the schema does not hold": func() {
			Schema(Table[child]("children", PrimaryKey("id"), References("parent_a", users)))
		},
	} {
		if got := panicOf(declare); !strings.Contains(got, want) {
			t.Errorf("panicked %q\nwant %q", got, want)
		}
	}
}

func panicOf(declare func()) (panicked string) {
	defer func() { panicked = fmt.Sprint(recover()) }()
	declare()
	return "nothing"
}

// what a declaration may say: a default of an untyped constant for a named
// type, a JSON default of the value it holds, a uuid kept as bytes, and a
// field shadowed by a shallower one of the same column
func TestADeclarationSaysWhatItsStructCannot(t *testing.T) {
	knowUUIDs(t)
	type ticket struct {
		ID       UUID
		Status   Status
		Level    Level
		Tags     JSON[[]string]
		Due      *Date
		Opened   time.Time
		Ratio    float64
		Deadline Date
	}
	sql := Schema(Table[ticket]("tickets",
		PrimaryKey("id"),
		Storage("id", Blob),
		Default("status", "open"),
		Default("level", 3),
		Default("tags", []string{"new"}),
		Default("due", nil),
		Default("opened", time.UnixMilli(1000)),
		Default("ratio", 1),
		Default("deadline", Date{2026, 12, 31}),
		DefaultSQL("level", "abs(random()) % 5"),
	)).SQL()
	for _, want := range []string{
		"id       BLOB NOT NULL PRIMARY KEY CHECK (length(id) = 16)",
		"status   TEXT NOT NULL DEFAULT 'open'",
		"level    INTEGER NOT NULL DEFAULT (abs(random()) % 5)",
		`tags     TEXT NOT NULL DEFAULT '["new"]' CHECK (json_valid(tags))`,
		"due      TEXT DEFAULT NULL CHECK (due IS date(due))",
		"opened   INTEGER NOT NULL DEFAULT 1000",
		"ratio    REAL NOT NULL DEFAULT 1",
		"deadline TEXT NOT NULL DEFAULT '2026-12-31' CHECK (deadline IS date(deadline))",
	} {
		if !strings.Contains(sql, want) {
			t.Errorf("%s\nlacks %q", sql, want)
		}
	}
	if got := modelOf(reflect.TypeFor[shadowed]()).fields; len(got) != 1 || got[0].name != "shadowed.Title" {
		t.Errorf("a shadowed field: %+v", got)
	}
}
