package catalog

import (
	"fmt"
	"strings"
)

// Draft is the migration that makes file what declared says, from their
// structural differences, and whether a person must finish it. A change the
// tool cannot decide, a rename or a type's conversion, leaves a TODO where SQL
// is missing, so that the draft fails until someone writes it:
//
//	INSERT INTO notes_new (id, status) SELECT id, /* TODO: … */ FROM notes;
func Draft(declared, file *Catalog, differences []Difference) (sql string, unfinished bool) {
	var order []string
	byTable := map[string][]Difference{}
	for _, d := range differences {
		if !d.Structural() {
			continue
		}
		if _, seen := byTable[d.Table]; !seen {
			order = append(order, d.Table)
		}
		byTable[d.Table] = append(byTable[d.Table], d)
	}

	var parts []string
	for _, name := range order {
		want, have := declared.Table(name), file.Table(name)
		var part string
		var todo bool
		switch {
		case have == nil:
			part = create(want)
		case rebuilds(want, have, byTable[name]):
			part, todo = rebuild(want, have, byTable[name])
		default:
			part, todo = alter(want, byTable[name])
		}
		parts = append(parts, part)
		unfinished = unfinished || todo
	}
	return strings.Join(parts, "\n"), unfinished
}

func create(want *Table) string {
	var out strings.Builder
	out.WriteString(want.SQL + ";\n")
	for _, ix := range want.Indexes {
		if ix.SQL != "" {
			out.WriteString(ix.SQL + ";\n")
		}
	}
	return out.String()
}

// rebuilds is a change ALTER TABLE cannot make: a column's type, NULL,
// default, the key, a reference, STRICT or rowids; a column it cannot add or
// drop, unless one gone beside one new may be a rename; a UNIQUE constraint
// the table's text holds
func rebuilds(want, have *Table, differences []Difference) bool {
	renaming := hasWhat(differences, MissingColumn) && hasWhat(differences, ExtraColumn)
	for _, d := range differences {
		switch d.What {
		case ColumnType, Nullability, DefaultValue, PrimaryKey, References, NotStrict, RowID:
			return true
		case MissingColumn:
			if !renaming && !addable(want, want.Column(d.Column)) {
				return true
			}
		case ExtraColumn:
			if !renaming && !droppable(have, d.Column) {
				return true
			}
		case ExtraIndex, IndexShape:
			if ix := have.index(d.Column); ix != nil && ix.SQL == "" {
				return true
			}
		default:
		}
	}
	return false
}

func hasWhat(differences []Difference, what What) bool {
	for _, d := range differences {
		if d.What == what {
			return true
		}
	}
	return false
}

// addable is a column ADD COLUMN takes: no part of the key, and a default
// that is a value, not NULL when the column is NOT NULL
func addable(t *Table, c *Column) bool {
	if containsFold(t.PrimaryKey, c.Name) {
		return false
	}
	value, isLiteral := literal(c.Default)
	if c.Default != "" && !isLiteral {
		return false
	}
	return !c.NotNull || (isLiteral && value != nil)
}

// droppable is a column DROP COLUMN drops once the indexes on it are gone: no
// part of the key, of a UNIQUE constraint or of a reference
func droppable(t *Table, column string) bool {
	if containsFold(t.PrimaryKey, column) {
		return false
	}
	for _, ix := range t.Indexes {
		if ix.SQL == "" && containsFold(ix.Columns, column) {
			return false
		}
	}
	for _, ref := range t.References {
		if containsFold(ref.From, column) {
			return false
		}
	}
	return true
}

// alter makes the changes ALTER TABLE can: indexes dropped first, columns
// added and dropped, indexes made last. A column gone beside one new may be a
// rename, which only a person can tell
func alter(want *Table, differences []Difference) (string, bool) {
	var dropped, added, drops, creates []string
	for _, d := range differences {
		switch d.What {
		case ExtraIndex:
			drops = append(drops, "DROP INDEX "+Quote(d.Column)+";")
		case IndexShape:
			drops = append(drops, "DROP INDEX "+Quote(d.Column)+";")
			creates = append(creates, want.index(d.Column).SQL+";")
		case MissingIndex:
			creates = append(creates, d.Declared+";")
		case ExtraColumn:
			dropped = append(dropped, d.Column)
		case MissingColumn:
			added = append(added, want.Column(d.Column).Definition)
		default:
		}
	}

	lines := drops
	table := Quote(want.Name)
	todo := len(dropped) > 0 && len(added) > 0
	switch {
	case todo:
		lines = append(lines, renameOrReplace(table, want.Name, dropped, added)...)
	default:
		for _, column := range dropped {
			lines = append(lines, fmt.Sprintf("ALTER TABLE %s DROP COLUMN %s;", table, Quote(column)))
		}
		for _, definition := range added {
			lines = append(lines, fmt.Sprintf("ALTER TABLE %s ADD COLUMN %s;", table, definition))
		}
	}
	lines = append(lines, creates...)
	return strings.Join(lines, "\n") + "\n", todo
}

// renameOrReplace asks whether the columns gone and the new ones are renames,
// in a statement that does not run until it is answered
func renameOrReplace(table, name string, dropped, added []string) []string {
	return []string{
		fmt.Sprintf("-- REVIEW: %s loses %s and gains %s. A rename keeps the rows' values, a drop loses them;",
			name, strings.Join(dropped, ", "), strings.Join(added, ", ")),
		"-- write which each is where the TODO stands, one ALTER TABLE a change.",
		fmt.Sprintf("ALTER TABLE %s /* TODO: RENAME COLUMN %s TO …, or DROP COLUMN %s and ADD COLUMN %s */;",
			table, dropped[0], dropped[0], added[0]),
	}
}

// rebuild is SQLite's own procedure for a change ALTER TABLE cannot make: a
// new table, the rows copied, the old one dropped and the new one renamed,
// its indexes and triggers made again. Foreign keys are off while migrations
// run, so the children of the table keep their rows and find the new one.
func rebuild(want, have *Table, differences []Difference) (string, bool) {
	fresh := want.Name + "_new"
	columns, values, todo := copied(want, have)

	var out strings.Builder
	fmt.Fprintf(&out, "-- REVIEW: SQLite cannot make this change in place; %s is rebuilt:\n", want.Name)
	for _, d := range differences {
		fmt.Fprintf(&out, "--   %s\n", d.Sentence("", Migrations))
	}
	for _, c := range have.Columns {
		if want.Column(c.Name) == nil {
			fmt.Fprintf(&out, "--   the copy leaves %s and its values out\n", c.Name)
		}
	}
	fmt.Fprintf(&out, "%s;\n", renamed(want.SQL, fresh))
	fmt.Fprintf(&out, "INSERT INTO %s (%s) SELECT %s FROM %s;\n",
		Quote(fresh), strings.Join(columns, ", "), strings.Join(values, ", "), Quote(want.Name))
	fmt.Fprintf(&out, "DROP TABLE %s;\n", Quote(want.Name))
	fmt.Fprintf(&out, "ALTER TABLE %s RENAME TO %s;\n", Quote(fresh), Quote(want.Name))
	for _, ix := range want.Indexes {
		if ix.SQL != "" {
			out.WriteString(ix.SQL + ";\n")
		}
	}
	for _, ix := range have.Indexes {
		if ix.SQL != "" && !ix.Plain() && want.index(ix.Name) == nil {
			out.WriteString(ix.SQL + ";\n")
		}
	}
	for _, trigger := range have.Triggers {
		out.WriteString(trigger + ";\n")
	}
	return out.String(), todo
}

// copied is the columns the rebuild copies and what each is copied from; a
// value the tool cannot decide is a TODO, which leaves the SELECT unfinished
func copied(want, have *Table) (columns, values []string, todo bool) {
	for _, c := range want.Columns {
		old := have.Column(c.Name)
		name := Quote(c.Name)
		switch {
		case old == nil && (!c.NotNull || c.Default != "" || rowid(want, c.Name)):
			continue
		case old == nil:
			values = append(values,
				fmt.Sprintf("/* TODO: what %s.%s holds for the rows before it */", want.Name, c.Name))
			todo = true
		case storage(old.Type) != storage(c.Type):
			values = append(values,
				fmt.Sprintf("/* TODO: how does each old %s, %s, become %s? */", c.Name, old.Type, c.Type))
			todo = true
		case c.NotNull && !old.NotNull && !rowid(want, c.Name):
			values = append(values, fmt.Sprintf("coalesce(%s, /* TODO: what a NULL %s becomes */)", name, c.Name))
			todo = true
		default:
			values = append(values, name)
		}
		columns = append(columns, name)
	}
	return columns, values, todo
}

// renamed is a CREATE TABLE's text with the table's name replaced
func renamed(create, name string) string {
	tokens := tokenize(create)
	seen := 0
	for i, t := range tokens {
		if t.kind == space {
			continue
		}
		if seen++; seen == 3 {
			tokens[i] = token{kind: identifier, text: Quote(name)}
			break
		}
	}
	var out strings.Builder
	for _, t := range tokens {
		out.WriteString(t.text)
	}
	return out.String()
}
