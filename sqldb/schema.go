package sqldb

import (
	"fmt"
	"strings"
)

// SchemaDef is a database's tables, in the order Schema was given them.
type SchemaDef struct {
	tables []*table
	sql    string
}

// Schema is what the migrations of one database make, and what Open checks the
// file against. It panics, as Table does, on two tables of one name and on a
// reference to a table it does not hold.
func Schema(tables ...AnyTable) *SchemaDef {
	schema := &SchemaDef{}
	named := map[string]bool{}
	for _, declared := range tables {
		t := declared.declared()
		if t == nil {
			panic("sqldb: schema: a table not declared yet")
		}
		if named[strings.ToLower(t.name)] {
			panic(fmt.Sprintf("sqldb: schema: two tables named %s", t.name))
		}
		named[strings.ToLower(t.name)] = true
		schema.tables = append(schema.tables, t)
	}
	blocks := make([]string, len(schema.tables))
	for i, t := range schema.tables {
		for _, c := range t.columns {
			if c.reference != nil && !named[strings.ToLower(c.reference.table)] {
				panic(fmt.Sprintf("sqldb: schema: %s.%s references %s, which the schema does not hold",
					t.name, c.name, c.reference.table))
			}
		}
		blocks[i] = t.ddl()
	}
	schema.sql = strings.Join(blocks, "\n")
	return schema
}

// SQL is every statement the schema stands for, as its migrations have to make
// it.
func (s *SchemaDef) SQL() string {
	return s.sql
}

// model is the Go type that declares a table, for the errors that name it
func (s *SchemaDef) model(table string) string {
	for _, t := range s.tables {
		if strings.EqualFold(t.name, table) {
			return t.model
		}
	}
	return ""
}
