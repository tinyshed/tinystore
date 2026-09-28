package catalog

import (
	"bytes"
	"fmt"
	"strings"
)

// What is the kind of one difference between a declared schema and a file.
type What int

const (
	MissingTable What = iota + 1
	MissingColumn
	ExtraColumn
	ColumnType
	Nullability
	DefaultValue
	PrimaryKey
	References
	MissingIndex
	ExtraIndex
	IndexShape
	NotStrict
	RowID

	// text, which a test reports and Open never compares
	CheckText
	DefaultText
	IndexName
)

// Difference is one way a file differs from the schema declared. Column is the
// column's or the index's name; Declared and File are how each side spells it,
// empty where one has nothing.
type Difference struct {
	What     What
	Table    string
	Column   string
	Declared string
	File     string
}

// Structural is a difference Open refuses a file for; the others are text
// spelled otherwise, or changed.
func (d Difference) Structural() bool {
	return d.What < CheckText
}

// Compare is how file differs from declared, table by table in declared's
// order; what declared does not name, a table, a trigger, a view, an index
// on an expression, is the file's own.
func Compare(declared, file *Catalog) []Difference {
	var found []Difference
	for _, want := range declared.Tables {
		have := file.Table(want.Name)
		if have == nil {
			found = append(found, Difference{What: MissingTable, Table: want.Name, Declared: want.SQL})
			continue
		}
		pair := tables{want: want, have: have, declared: declared, file: file}
		found = append(found, pair.compare()...)
	}
	return found
}

// tables is one table as declared and as the file has it
type tables struct {
	want, have     *Table
	declared, file *Catalog
}

func (p tables) compare() []Difference {
	var found []Difference
	if p.want.Strict && !p.have.Strict {
		found = append(found, p.differ(NotStrict, "", "STRICT", "not STRICT"))
	}
	if p.want.WithoutRowID != p.have.WithoutRowID {
		found = append(found, p.differ(RowID, "", rowidOf(p.want), rowidOf(p.have)))
	}
	found = append(found, p.columns()...)
	if !sameNames(p.want.PrimaryKey, p.have.PrimaryKey) {
		found = append(found, p.differ(PrimaryKey, "", keyOf(p.want.PrimaryKey), keyOf(p.have.PrimaryKey)))
	}
	found = append(found, p.references()...)
	found = append(found, p.indexes()...)
	return append(found, p.checks()...)
}

func (p tables) differ(what What, column, declared, file string) Difference {
	return Difference{What: what, Table: p.want.Name, Column: column, Declared: declared, File: file}
}

func (p tables) columns() []Difference {
	var found []Difference
	for _, want := range p.want.Columns {
		have := p.have.Column(want.Name)
		if have == nil {
			found = append(found, p.differ(MissingColumn, want.Name, want.Definition, ""))
			continue
		}
		if storage(want.Type) != storage(have.Type) {
			found = append(found, p.differ(ColumnType, want.Name, want.Type, have.Type))
		}
		numbered := rowid(p.want, want.Name) && rowid(p.have, have.Name)
		if want.NotNull != have.NotNull && !numbered {
			found = append(found, p.differ(Nullability, want.Name,
				nullability(want.NotNull), nullability(have.NotNull)))
		}
		if what, differs := defaults(want.Default, have.Default); differs {
			found = append(found, p.differ(what, want.Name, defaultOf(want.Default), defaultOf(have.Default)))
		}
	}
	for _, have := range p.have.Columns {
		if p.want.Column(have.Name) == nil {
			found = append(found, p.differ(ExtraColumn, have.Name, "", have.Definition))
		}
	}
	return found
}

// storage is a STRICT column's type as SQLite stores it: INT and INTEGER alike
func storage(declared string) string {
	if declared == "INT" {
		return "INTEGER"
	}
	return declared
}

// rowid is a column SQLite numbers the rows by, never NULL however declared:
// the INTEGER PRIMARY KEY of a table with rowids
func rowid(t *Table, column string) bool {
	c := t.Column(column)
	return c != nil && !t.WithoutRowID && len(t.PrimaryKey) == 1 && strings.EqualFold(t.PrimaryKey[0], column) &&
		c.Type == "INTEGER"
}

// defaults compares two defaults: values by what SQLite makes of their text,
// so that 00 is 0; expressions by their text, folded
func defaults(want, have string) (What, bool) {
	wantValue, wantLiteral := literal(want)
	haveValue, haveLiteral := literal(have)
	switch {
	case (want == "" || (wantLiteral && wantValue == nil)) && (have == "" || (haveLiteral && haveValue == nil)):
		return 0, false
	case want == "" || have == "" || wantLiteral != haveLiteral:
		return DefaultValue, true
	case wantLiteral:
		return DefaultValue, !sameValue(wantValue, haveValue)
	}
	return DefaultText, Fold(want) != Fold(have)
}

func sameValue(a, b any) bool {
	switch x := a.(type) {
	case int64:
		switch y := b.(type) {
		case int64:
			return x == y
		case float64:
			return float64(x) == y
		}
	case float64:
		switch y := b.(type) {
		case int64:
			return x == float64(y)
		case float64:
			return x == y
		}
	case string:
		y, ok := b.(string)
		return ok && x == y
	case []byte:
		y, ok := b.([]byte)
		return ok && bytes.Equal(x, y)
	}
	return false
}

func (p tables) references() []Difference {
	want, have := referencesOf(p.want, p.declared), referencesOf(p.have, p.file)
	var found []Difference
	for _, ref := range p.want.References {
		from := strings.ToLower(strings.Join(ref.From, ", "))
		if want[from] != have[from] {
			found = append(found, p.differ(References, strings.Join(ref.From, ", "), want[from], have[from]))
		}
	}
	for _, ref := range p.have.References {
		from := strings.ToLower(strings.Join(ref.From, ", "))
		if _, declared := want[from]; !declared {
			found = append(found, p.differ(References, strings.Join(ref.From, ", "), "", have[from]))
		}
	}
	return found
}

// referencesOf spells each reference of t by the columns it is from:
//
//	author_id → users (id) ON DELETE CASCADE
func referencesOf(t *Table, owner *Catalog) map[string]string {
	spelled := map[string]string{}
	for _, ref := range t.References {
		to := ref.To
		if parent := owner.Table(ref.Table); len(to) == 0 && parent != nil {
			to = parent.PrimaryKey
		}
		text := fmt.Sprintf("%s (%s)", strings.ToLower(ref.Table), strings.ToLower(strings.Join(to, ", ")))
		if ref.OnDelete != "" && ref.OnDelete != "NO ACTION" {
			text += " ON DELETE " + ref.OnDelete
		}
		if ref.OnUpdate != "" && ref.OnUpdate != "NO ACTION" {
			text += " ON UPDATE " + ref.OnUpdate
		}
		spelled[strings.ToLower(strings.Join(ref.From, ", "))] = text
	}
	return spelled
}

// indexes pairs the declared indexes with the file's by name, then by shape,
// so that an index a migration named otherwise still counts
func (p tables) indexes() []Difference {
	var found []Difference
	claimed := map[*Index]bool{}
	var unnamed []*Index
	for _, want := range p.want.Indexes {
		if want.Origin == "pk" {
			continue
		}
		have := p.have.index(want.Name)
		if have == nil {
			unnamed = append(unnamed, want)
			continue
		}
		claimed[have] = true
		if !sameShape(want, have) {
			found = append(found, p.differ(IndexShape, want.Name, shapeOf(want), shapeOf(have)))
		}
	}
	for _, want := range unnamed {
		if have := p.have.shaped(want, claimed); have != nil {
			claimed[have] = true
			found = append(found, p.differ(IndexName, want.Name, want.Name, have.Name))
			continue
		}
		found = append(found, p.differ(MissingIndex, want.Name, want.SQL, ""))
	}
	for _, have := range p.have.Indexes {
		if !claimed[have] && have.Origin != "pk" && have.Plain() {
			found = append(found, p.differ(ExtraIndex, have.Name, "", indexText(have, p.have.Name)))
		}
	}
	return found
}

func (t *Table) index(name string) *Index {
	for _, ix := range t.Indexes {
		if strings.EqualFold(ix.Name, name) {
			return ix
		}
	}
	return nil
}

func (t *Table) shaped(want *Index, claimed map[*Index]bool) *Index {
	for _, ix := range t.Indexes {
		if !claimed[ix] && ix.Origin != "pk" && ix.Plain() && sameShape(want, ix) {
			return ix
		}
	}
	return nil
}

func sameShape(a, b *Index) bool {
	return a.Unique == b.Unique && a.Partial == b.Partial && sameNames(a.Columns, b.Columns)
}

func shapeOf(ix *Index) string {
	unique := ""
	if ix.Unique {
		unique = "UNIQUE "
	}
	shape := fmt.Sprintf("%s(%s)", unique, strings.Join(ix.Columns, ", "))
	if ix.Partial {
		shape += " WHERE …"
	}
	return shape
}

// indexText is how a file made an index: its CREATE INDEX, or the constraint
// of its table
func indexText(ix *Index, table string) string {
	if ix.SQL != "" {
		return ix.SQL
	}
	return fmt.Sprintf("%s, of a UNIQUE constraint in %s", shapeOf(ix), table)
}

// checks compares the two tables' CHECKs as text folded, each spelling once
func (p tables) checks() []Difference {
	have := map[string]int{}
	for _, check := range p.have.Checks {
		have[Fold(check)]++
	}
	var found []Difference
	for _, check := range p.want.Checks {
		if folded := Fold(check); have[folded] > 0 {
			have[folded]--
			continue
		}
		found = append(found, p.differ(CheckText, "", check, ""))
	}
	for _, check := range p.have.Checks {
		if folded := Fold(check); have[folded] > 0 {
			have[folded]--
			found = append(found, p.differ(CheckText, "", "", check))
		}
	}
	return found
}

func sameNames(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if !strings.EqualFold(a[i], b[i]) {
			return false
		}
	}
	return true
}

func nullability(notNull bool) string {
	if notNull {
		return "NOT NULL"
	}
	return "nullable"
}

func rowidOf(t *Table) string {
	if t.WithoutRowID {
		return "WITHOUT ROWID"
	}
	return "a table with rowids"
}

func keyOf(columns []string) string {
	if len(columns) == 0 {
		return "none"
	}
	return "(" + strings.Join(columns, ", ") + ")"
}

func defaultOf(text string) string {
	if text == "" {
		return "nothing"
	}
	return text
}
