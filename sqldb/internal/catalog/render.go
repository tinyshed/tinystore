package catalog

import (
	"fmt"
	"strings"
)

// Side is what a schema is compared with, and the verbs it takes.
type Side struct{ name, has, does string }

var (
	File       = Side{name: "the file", has: "has", does: "does"}
	Migrations = Side{name: "the migrations", has: "make", does: "do"}
)

// Sentence says a difference as Open reports it; model is the Go type that
// declares the table, "" when there is none to name:
//
//	notes.description is declared in Note, and the file has no such column; is a migration missing?
func (d Difference) Sentence(model string, side Side) string {
	in, holder := "", "the schema"
	if model != "" {
		in, holder = " in "+model, model
	}
	column := d.Table + "." + d.Column
	switch d.What {
	case MissingTable:
		return fmt.Sprintf("%s is declared%s, and %s %s no such table; is a migration missing?",
			d.Table, in, side.name, side.has)
	case MissingColumn:
		return fmt.Sprintf("%s is declared%s, and %s %s no such column; is a migration missing?",
			column, in, side.name, side.has)
	case ExtraColumn:
		return fmt.Sprintf("%s is in %s, and %s has no field for it", column, side.name, holder)
	case ColumnType, Nullability:
		return fmt.Sprintf("%s is %s in the schema and %s in %s", column, d.Declared, d.File, side.name)
	case DefaultValue, DefaultText:
		return fmt.Sprintf("%s defaults to %s in the schema and to %s in %s", column, d.Declared, d.File, side.name)
	case PrimaryKey:
		return fmt.Sprintf("%s has the primary key %s in the schema and %s in %s",
			d.Table, d.Declared, d.File, side.name)
	case References:
		return fmt.Sprintf("%s refers to %s in the schema and to %s in %s",
			column, orNothing(d.Declared), orNothing(d.File), side.name)
	case MissingIndex:
		return fmt.Sprintf("the index %s on %s is declared, and %s %s no such index",
			d.Column, d.Table, side.name, side.has)
	case ExtraIndex:
		return fmt.Sprintf("%s %s the index %s on %s, which the schema does not declare",
			side.name, side.has, d.Column, d.Table)
	case IndexShape:
		return fmt.Sprintf("the index %s is %s in the schema and %s in %s", d.Column, d.Declared, d.File, side.name)
	case NotStrict, RowID:
		return fmt.Sprintf("%s is %s in the schema and %s in %s", d.Table, d.Declared, d.File, side.name)
	case CheckText:
		if d.File == "" {
			return fmt.Sprintf("%s checks %s in the schema, and %s %s not; spelled otherwise, or changed?",
				d.Table, d.Declared, side.name, side.does)
		}
		return fmt.Sprintf("%s checks %s in %s, and the schema does not; spelled otherwise, or changed?",
			d.Table, d.File, side.name)
	case IndexName:
		return fmt.Sprintf("the index %s on %s is %s in %s", d.Column, d.Table, d.File, side.name)
	}
	return fmt.Sprintf("%s differs", d.Table)
}

// Line is a difference as a test lists them: + for what the schema declares
// and the migrations do not make, - for the reverse, ~ for what both have
// otherwise
//
//	a column new:     + notes.description TEXT
//	an index gone:    - INDEX notes_title ON notes (title)
func (d Difference) Line() string {
	switch d.What {
	case MissingTable:
		return "+ TABLE " + d.Table
	case MissingColumn:
		return "+ " + d.Table + "." + d.Declared
	case ExtraColumn:
		return "- " + d.Table + "." + d.File
	case MissingIndex:
		return "+ " + strings.TrimPrefix(d.Declared, "CREATE ")
	case ExtraIndex:
		return "- " + strings.TrimPrefix(d.File, "CREATE ")
	}
	return "~ " + d.Sentence("", Migrations)
}

func orNothing(text string) string {
	if text == "" {
		return "nothing"
	}
	return text
}
