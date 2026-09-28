package sqldb

import (
	"errors"
	"fmt"
	"reflect"
	"slices"
	"strings"
)

// StorageClass is the STRICT column a custom type, or a uuid, is kept in.
type StorageClass int

const (
	Text StorageClass = iota + 1
	Integer
	Real
	Blob
)

func (s StorageClass) String() string {
	return [...]string{"", "TEXT", "INTEGER", "REAL", "BLOB"}[s]
}

// Action is what a reference does when the row it refers to is deleted.
type Action int

const (
	Cascade Action = iota + 1
	SetNull
)

func (a Action) String() string {
	return [...]string{"NO ACTION", "CASCADE", "SET NULL"}[a]
}

// TableDef is a table declared for the rows of T: what Schema prints, what
// Open checks the file against and what Insert writes. Table makes it.
type TableDef[T any] struct {
	table    *table
	insert   string    // INSERT …, RETURNING what the database generates but a rowid
	written  []*column // what Insert writes, the generated columns left out
	numbered *column   // a generated rowid, which the insert's result carries
	returned []*column // the generated columns the insert returns
}

// AnyTable is a table of any row type, as Schema and References take it.
type AnyTable interface{ declared() *table }

func (d *TableDef[T]) declared() *table {
	if d == nil {
		return nil
	}
	return d.table
}

// table is what one declaration says, as a value of its own: the DDL, Open's
// check and the migrations drafted are made from it rather than from Go
type table struct {
	name       string
	model      string // the Go type declaring it: Note
	columns    []*column
	primaryKey []string
	indexes    []*index
	checks     []string
}

type column struct {
	name      string
	logical   kind
	storage   StorageClass
	nullable  bool
	generated bool
	length    int    // bytes a [N]byte, or a uuid kept as bytes, holds
	def       string // DEFAULT's text: 0, '[]', (lower(hex(randomblob(4))))
	reference *reference
	field     *field
}

type reference struct {
	table  string
	column string
	action Action
}

type index struct {
	name    string
	columns []string
	unique  bool
}

// TableOption says what a struct cannot: keys, indexes, references, defaults,
// checks and storage.
type TableOption func(*table) error

// Table declares the table name for the rows of T, and panics, as
// regexp.MustCompile does, on a declaration that cannot be a table: an option
// naming a column T lacks, a default of another type, a generated column
// nothing fills, a field sqldb cannot store, a custom type without Storage, a
// reference to a table keyed by several columns.
func Table[T any](name string, options ...TableOption) *TableDef[T] {
	declared, err := declare(reflect.TypeFor[T](), name, options)
	if err != nil {
		panic(fmt.Sprintf("sqldb: table %s: %v", name, err))
	}
	def := &TableDef[T]{table: declared}
	for _, c := range declared.columns {
		switch {
		case !c.generated:
			def.written = append(def.written, c)
		case declared.rowid(c):
			def.numbered = c
		default:
			def.returned = append(def.returned, c)
		}
	}
	if def.numbered != nil && len(def.returned) > 0 {
		def.returned, def.numbered = append([]*column{def.numbered}, def.returned...), nil
	}
	def.insert = insertQuery(declared.name, def.written, def.returned)
	return def
}

func declare(t reflect.Type, name string, options []TableOption) (*table, error) {
	if err := checkTableName(name); err != nil {
		return nil, err
	}
	if !isRecord(t) {
		return nil, fmt.Errorf("%s is not a struct whose fields are columns", t)
	}
	m := modelOf(t)
	if m.err != nil {
		return nil, m.err
	}
	declared := &table{name: name, model: t.Name()}
	for i := range m.fields {
		c, err := columnOf(&m.fields[i])
		if err != nil {
			return nil, err
		}
		declared.columns = append(declared.columns, c)
	}
	for _, option := range options {
		if err := option(declared); err != nil {
			return nil, err
		}
	}
	return declared, declared.finish()
}

func checkTableName(name string) error {
	lower := strings.ToLower(name)
	switch {
	case name == "":
		return errors.New("a table needs a name")
	case strings.HasPrefix(lower, "sqlite_"), lower == "_tinystore_migrations":
		return fmt.Errorf("%s is a name SQLite or sqldb keeps for itself", name)
	}
	return nil
}

func columnOf(f *field) (*column, error) {
	if f.valueErr != nil {
		return nil, fmt.Errorf("column %s: %s: %w", f.column, f.name, f.valueErr)
	}
	c := &column{
		name: f.column, logical: f.value.kind, storage: f.value.kind.storage(),
		nullable: f.value.wrap != bare, generated: f.generated, field: f,
	}
	if f.value.kind == kindArray {
		c.length = f.value.length
	}
	return c, nil
}

// finish checks what only the whole declaration shows
func (t *table) finish() error {
	for _, c := range t.columns {
		switch {
		case c.storage == 0:
			return fmt.Errorf("%s.%s: sqldb does not know how %s is stored; "+
				"say sqldb.Storage(%q, sqldb.Text), or Integer, Real, Blob", t.name, c.name, c.field.typ, c.name)
		case c.generated && c.def == "" && !t.rowid(c):
			return fmt.Errorf("%s.%s is generated, and nothing fills it: "+
				"an INTEGER PRIMARY KEY, a Default or a DefaultSQL does", t.name, c.name)
		}
	}
	names := map[string]bool{}
	for _, ix := range t.indexes {
		if names[strings.ToLower(ix.name)] {
			return fmt.Errorf("two indexes named %s", ix.name)
		}
		names[strings.ToLower(ix.name)] = true
	}
	return nil
}

// rowid is the column SQLite numbers the rows by: a table's one-column
// INTEGER PRIMARY KEY
func (t *table) rowid(c *column) bool {
	return len(t.primaryKey) == 1 && t.primaryKey[0] == c.name && c.storage == Integer && c.logical == kindInteger
}

func (t *table) column(name string) (*column, error) {
	for _, c := range t.columns {
		if c.name == name {
			return c, nil
		}
	}
	return nil, fmt.Errorf("%s has no column %s", t.name, name)
}

func (t *table) knows(columns []string) error {
	if len(columns) == 0 {
		return errors.New("a key or an index of no columns")
	}
	for i, name := range columns {
		if _, err := t.column(name); err != nil {
			return err
		}
		if slices.Contains(columns[:i], name) {
			return fmt.Errorf("%s twice in (%s)", name, strings.Join(columns, ", "))
		}
	}
	return nil
}

// PrimaryKey is the table's key; a single INTEGER column is the rowid SQLite
// numbers new rows by.
func PrimaryKey(columns ...string) TableOption {
	return func(t *table) error {
		if len(t.primaryKey) > 0 {
			return errors.New("two primary keys")
		}
		if err := t.knows(columns); err != nil {
			return err
		}
		t.primaryKey = slices.Clone(columns)
		return nil
	}
}

// Unique is a unique index named <table>_<columns>, rather than a table
// constraint, so that a later migration can drop it without rebuilding the
// table.
func Unique(columns ...string) TableOption {
	return indexOn("", columns, true)
}

// Index is an index named <table>_<columns>.
func Index(columns ...string) TableOption {
	return indexOn("", columns, false)
}

// NamedIndex is an index a migration made under a name of its own.
func NamedIndex(name string, columns ...string) TableOption {
	return indexOn(name, columns, false)
}

func indexOn(name string, columns []string, unique bool) TableOption {
	return func(t *table) error {
		if err := t.knows(columns); err != nil {
			return err
		}
		if name == "" {
			name = t.name + "_" + strings.Join(columns, "_")
		}
		t.indexes = append(t.indexes, &index{name: name, columns: slices.Clone(columns), unique: unique})
		return nil
	}
}

// References points column at the primary key of parent, which must be one
// column of the same storage; the action, when given, is ON DELETE's.
func References(column string, parent AnyTable, action ...Action) TableOption {
	return func(t *table) error {
		child, err := t.column(column)
		if err != nil {
			return err
		}
		target := parent.declared()
		if target == nil {
			return fmt.Errorf("%s references a table not declared yet", column)
		}
		if len(target.primaryKey) != 1 {
			return fmt.Errorf("%s references %s, whose primary key is (%s); this version refers to a key of one column",
				column, target.name, strings.Join(target.primaryKey, ", "))
		}
		key, err := target.column(target.primaryKey[0])
		if err != nil {
			return err
		}
		return child.refer(key, target.name, action)
	}
}

func (c *column) refer(key *column, table string, action []Action) error {
	switch {
	case c.reference != nil:
		return fmt.Errorf("%s references twice", c.name)
	case len(action) > 1:
		return fmt.Errorf("%s: one action on delete, not %d", c.name, len(action))
	case key.storage != c.storage:
		return fmt.Errorf("%s is %s and %s.%s %s", c.name, c.storage, table, key.name, key.storage)
	}
	c.reference = &reference{table: table, column: key.name}
	if len(action) == 1 {
		c.reference.action = action[0]
	}
	if c.reference.action == SetNull && !c.nullable {
		return fmt.Errorf("%s is set to NULL on delete, and %s cannot be NULL", c.name, c.field.typ)
	}
	return nil
}

// Default is the value SQL gives column when an insert leaves it out, and
// what lets a later migration add a NOT NULL column to a table holding rows.
// Insert writes every field, so it never uses one. value is of the field's
// type, or of the type a pointer, a sql.Null or a sqldb.JSON holds.
func Default(column string, value any) TableOption {
	return func(t *table) error {
		c, err := t.column(column)
		if err != nil {
			return err
		}
		if c.def, err = c.literal(value); err != nil {
			return fmt.Errorf("the default of %s: %w", column, err)
		}
		return nil
	}
}

// DefaultSQL is an expression SQL computes for column when an insert leaves it
// out, such as lower(hex(randomblob(4))).
func DefaultSQL(column, expression string) TableOption {
	return func(t *table) error {
		c, err := t.column(column)
		if err != nil {
			return err
		}
		if strings.TrimSpace(expression) == "" {
			return fmt.Errorf("an empty default for %s", column)
		}
		c.def = "(" + expression + ")"
		return nil
	}
}

// Check is a condition every row keeps, as SQL spells it.
func Check(expression string) TableOption {
	return func(t *table) error {
		if strings.TrimSpace(expression) == "" {
			return errors.New("an empty check")
		}
		t.checks = append(t.checks, expression)
		return nil
	}
}

// Storage says how column keeps a type sqldb does not know, whose own Scan and
// Value convert it; or keeps a uuid as its 16 bytes, which a raw query then
// passes as id[:].
func Storage(column string, class StorageClass) TableOption {
	return func(t *table) error {
		c, err := t.column(column)
		if err != nil {
			return err
		}
		switch {
		case class < Text || class > Blob:
			return fmt.Errorf("a storage of %d for %s", class, column)
		case c.logical == kindUUID && (class == Text || class == Blob):
			c.storage = class
			if class == Blob {
				c.length = 16
			}
		case c.logical == kindCustom:
			c.storage = class
		default:
			return fmt.Errorf("%s is a %s, which sqldb stores as %s; Storage is for a type it does not know, or a uuid",
				column, c.field.typ, c.storage)
		}
		return nil
	}
}
