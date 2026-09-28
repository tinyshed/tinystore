package wire

import (
	"maps"
	"slices"
	"time"
	"unicode/utf8"
)

// The sql engine's methods, docs/wire.md#sql.
const (
	SQLOpen  Method = 0x0401
	SQLExec  Method = 0x0402
	SQLQuery Method = 0x0403
	SQLBatch Method = 0x0404
)

// SQLDatabase is sql.open's request: a database by name, and the migrations
// its file must have applied, which an admin's first open applies.
type SQLDatabase struct {
	Name       string
	Migrations []SQLMigration
}

// SQLMigration is one .sql file of a database's migrations.
type SQLMigration struct {
	Name string
	Text string
}

func (d SQLDatabase) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Str(1, d.Name)
	if len(d.Migrations) > 0 {
		m.Key(2)
		buf := AppendArray(m.Buf(), len(d.Migrations))
		for _, migration := range d.Migrations {
			file := BeginMap(buf)
			file.Str(1, migration.Name)
			file.Str(2, migration.Text)
			buf = file.End()
		}
		m.SetBuf(buf)
	}
	return m.End()
}

func (d *SQLDatabase) Decode(body []byte) error {
	dec := NewDecoder(body)
	for key := range dec.Fields() {
		switch key {
		case 1:
			d.Name = dec.Str()
		case 2:
			for range dec.Items() {
				var migration SQLMigration
				for field := range dec.Fields() {
					switch field {
					case 1:
						migration.Name = dec.Str()
					case 2:
						migration.Text = dec.Str()
					}
				}
				d.Migrations = append(d.Migrations, migration)
			}
		}
	}
	return dec.End()
}

// SQLStatement is one statement with its arguments: positional, named, or
// both, each nil, an integer, a float, str, bin or a bool. Write runs a
// query on the writer, for a write whose returning clause gives rows; Rows
// asks a batch's statement for the rows it returns.
type SQLStatement struct {
	Handle uint64
	SQL    string
	Args   []any
	Named  map[string]any
	Write  bool
	Rows   bool
}

func (s SQLStatement) Append(dst []byte) []byte {
	m := BeginMap(dst)
	if s.Handle != 0 {
		m.Uint(1, s.Handle)
	}
	s.appendFields(&m)
	return m.End()
}

func (s SQLStatement) appendFields(m *Map) {
	m.Str(2, s.SQL)
	if len(s.Args) > 0 {
		m.Key(3)
		buf := AppendArray(m.Buf(), len(s.Args))
		for _, arg := range s.Args {
			buf = AppendSQLValue(buf, arg)
		}
		m.SetBuf(buf)
	}
	if len(s.Named) > 0 {
		m.Key(4)
		m.SetBuf(appendNamedValues(m.Buf(), s.Named))
	}
	if s.Write {
		m.Bool(5, true)
	}
	if s.Rows {
		m.Bool(6, true)
	}
}

func (s *SQLStatement) Decode(body []byte) error {
	d := NewDecoder(body)
	s.decode(&d)
	return d.End()
}

func (s *SQLStatement) decode(d *Decoder) {
	for key := range d.Fields() {
		switch key {
		case 1:
			s.Handle = d.Uint()
		case 2:
			s.SQL = d.Str()
		case 3:
			for range d.Items() {
				s.Args = append(s.Args, d.SQLValue())
			}
		case 4:
			s.Named = map[string]any{}
			for name := range d.Names() {
				s.Named[name] = d.SQLValue()
			}
		case 5:
			s.Write = d.Bool()
		case 6:
			s.Rows = d.Bool()
		}
	}
}

// SQLValue reads one of SQLite's five, or a bool: nil, an int64, a float64,
// a string or a []byte aliasing the body. An integer past int64 is refused,
// as SQLite would keep it as a REAL.
func (d *Decoder) SQLValue() any {
	switch d.Type() {
	case TypeNil:
		d.Nil()
		return nil
	case TypeBool:
		return d.Bool()
	case TypeInt:
		return d.Int()
	case TypeFloat:
		return d.Float()
	case TypeStr:
		return d.Str()
	case TypeBin:
		return d.Bin()
	}
	d.fail("%s where an SQL value belongs", d.Type())
	return nil
}

// AppendSQLValue writes a value as SQLite returned it: TEXT that is not UTF-8
// as bin, and a time the driver read from a column declared as one in
// SQLite's own spelling.
func AppendSQLValue(dst []byte, value any) []byte {
	switch v := value.(type) {
	case nil:
		return AppendNil(dst)
	case int64:
		return AppendInt(dst, v)
	case float64:
		return AppendFloat(dst, v)
	case bool:
		return AppendBool(dst, v)
	case string:
		if utf8.ValidString(v) {
			return AppendStr(dst, v)
		}
		return AppendBin(dst, []byte(v))
	case []byte:
		return AppendBin(dst, v)
	case time.Time:
		return AppendStr(dst, v.Format("2006-01-02 15:04:05.999999999-07:00"))
	}
	return AppendNil(dst)
}

func appendNamedValues(dst []byte, named map[string]any) []byte {
	dst = AppendMap(dst, len(named))
	for _, name := range sortedNames(named) {
		dst = AppendSQLValue(AppendStr(dst, name), named[name])
	}
	return dst
}

// SQLDone answers sql.exec, and is a write's result in a batch: the rows it
// changed and the rowid of the last it inserted.
type SQLDone struct {
	Changes int64
	LastID  int64
}

func (r SQLDone) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Int(1, r.Changes)
	m.Int(2, r.LastID)
	return m.End()
}

func (r *SQLDone) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			r.Changes = d.Int()
		case 2:
			r.LastID = d.Int()
		}
	}
	return d.End()
}

// SQLColumns begins sql.query's download: the columns' names, which every
// row that follows holds in their order.
type SQLColumns struct {
	Columns []string
}

func (c SQLColumns) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Key(1)
	m.SetBuf(appendStrs(m.Buf(), c.Columns))
	return m.End()
}

func (c *SQLColumns) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		if key == 1 {
			c.Columns = d.Strs()
		}
	}
	return d.End()
}

// SQLRow is one row of a query's download: its values, a column each.
type SQLRow struct {
	Values []any
}

func (r SQLRow) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Key(1)
	buf := AppendArray(m.Buf(), len(r.Values))
	for _, value := range r.Values {
		buf = AppendSQLValue(buf, value)
	}
	m.SetBuf(buf)
	return m.End()
}

func (r *SQLRow) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		if key != 1 {
			continue
		}
		for range d.Items() {
			value := d.SQLValue()
			if raw, ok := value.([]byte); ok {
				value = clone(raw)
			}
			r.Values = append(r.Values, value)
		}
	}
	return d.End()
}

// SQLStatements is sql.batch's request: statements one transaction runs, all
// or none, or with Read, reads from one snapshot.
type SQLStatements struct {
	Handle     uint64
	Statements []SQLStatement
	Read       bool
}

func (s SQLStatements) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Uint(1, s.Handle)
	m.Key(2)
	buf := AppendArray(m.Buf(), len(s.Statements))
	for _, statement := range s.Statements {
		fields := BeginMap(buf)
		statement.appendFields(&fields)
		buf = fields.End()
	}
	m.SetBuf(buf)
	if s.Read {
		m.Bool(3, true)
	}
	return m.End()
}

func (s *SQLStatements) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			s.Handle = d.Uint()
		case 2:
			for range d.Items() {
				var statement SQLStatement
				statement.decode(&d)
				s.Statements = append(s.Statements, statement)
			}
		case 3:
			s.Read = d.Bool()
		}
	}
	return d.End()
}

// SQLResults answers a batch: a result a statement, its changes and last
// rowid, and its columns and rows when it returned rows.
type SQLResults struct {
	Results []SQLResult
}

type SQLResult struct {
	SQLDone
	Columns []string
	Rows    [][]any
}

func (r SQLResults) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Key(1)
	buf := AppendArray(m.Buf(), len(r.Results))
	for _, result := range r.Results {
		buf = result.append(buf)
	}
	m.SetBuf(buf)
	return m.End()
}

func (r SQLResult) append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Int(1, r.Changes)
	m.Int(2, r.LastID)
	if r.Columns != nil {
		m.Key(3)
		m.SetBuf(appendStrs(m.Buf(), r.Columns))
		m.Key(4)
		buf := AppendArray(m.Buf(), len(r.Rows))
		for _, row := range r.Rows {
			buf = SQLRow{Values: row}.appendValues(buf)
		}
		m.SetBuf(buf)
	}
	return m.End()
}

func (r SQLRow) appendValues(dst []byte) []byte {
	dst = AppendArray(dst, len(r.Values))
	for _, value := range r.Values {
		dst = AppendSQLValue(dst, value)
	}
	return dst
}

func (r *SQLResults) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		if key != 1 {
			continue
		}
		for range d.Items() {
			var result SQLResult
			result.decode(&d)
			r.Results = append(r.Results, result)
		}
	}
	return d.End()
}

func (r *SQLResult) decode(d *Decoder) {
	for key := range d.Fields() {
		switch key {
		case 1:
			r.Changes = d.Int()
		case 2:
			r.LastID = d.Int()
		case 3:
			r.Columns = d.Strs()
		case 4:
			r.Rows = [][]any{}
			for range d.Items() {
				var row []any
				for range d.Items() {
					value := d.SQLValue()
					if raw, ok := value.([]byte); ok {
						value = clone(raw)
					}
					row = append(row, value)
				}
				r.Rows = append(r.Rows, row)
			}
		}
	}
}

func sortedNames[V any](named map[string]V) []string {
	return slices.Sorted(maps.Keys(named))
}
