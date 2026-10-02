package server

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"

	"github.com/tinyshed/tinystore/sqldb"
)

// errDataConnection is a statement, or a schema's change, that a data
// connection's capability does not allow
var errDataConnection = errors.New("a data connection changes rows, not the schema")

// errAdminOnly refuses a repair, or a stop, to a connection that is not an
// admin's.
var errAdminOnly = errors.New("only an admin connection can do this")

// errStopsWithItsProgram refuses a stop to a server whose program did not say
// how it stops.
var errStopsWithItsProgram = errors.New("server: this server stops with its program, not at a client's request")

func refused(format string, args ...any) error {
	return fmt.Errorf("%w: "+format, append([]any{errDataConnection}, args...)...)
}

// checkDataSQL lets a data connection's statement run only when both lines
// pass: readDataSQL over SQLite's own tokens, then explainDataSQL over the
// program SQLite compiles the statement to.
//
// Where the statement ends is the first line's alone. SQLite runs every
// statement of a string, EXPLAIN's included, and compiling a PRAGMA already
// changes its connection, so the second line must only ever see one statement
// that begins with a data verb, or nothing.
func checkDataSQL(ctx context.Context, db *sqldb.DB, statement string, args []any) error {
	if err := readDataSQL(statement); err != nil {
		return err
	}
	return explainDataSQL(ctx, db, statement, args)
}

// An allow-list: any other first word, EXPLAIN and PRAGMA included, is refused.
var dataVerbs = map[string]bool{
	"SELECT": true, "VALUES": true, "WITH": true, "INSERT": true, "REPLACE": true, "UPDATE": true, "DELETE": true,
}

// readDataSQL is the first line. It passes one statement that begins with a
// data verb, leaves nothing unclosed, and names neither sqlite_dbpage nor a
// pragma's table.
func readDataSQL(statement string) error {
	if at := strings.IndexByte(statement, 0); at >= 0 {
		return refused("a NUL byte at %d, past which SQLite reads nothing", at)
	}
	tokens := tokensOf(statement)
	if len(tokens) == 0 {
		return refused("no statement")
	}
	if first := tokens[0]; first.kind != tokenWord || !dataVerbs[upperASCII(first.text)] {
		return refused("a statement that begins %s; a data connection runs one that begins SELECT, VALUES, WITH, "+
			"INSERT, REPLACE, UPDATE or DELETE", clip(first.text))
	}
	for i, token := range tokens {
		switch {
		case token.kind == tokenIllegal:
			return refused("%s, which SQLite reads as no token or which runs unclosed to the end", clip(token.text))
		case token.kind == tokenSemi && i < len(tokens)-1:
			return refused("a second statement, %s after the first ;", clip(tokens[i+1].text))
		case isReservedName(token):
			return refused("the name %s, whose table writes the file's pages or changes the schema", clip(token.text))
		}
	}
	return nil
}

// isReservedName says whether token names sqlite_dbpage or a pragma's table.
//
// sqlite_dbpage writes a file's pages. A pragma's table, pragma_optimize among
// them, may analyze and so make tables.
//
// A string counts as a name, since SQLite takes one for a name where a name
// may stand:
//
//	select * from 'pragma_optimize'
func isReservedName(token sqlToken) bool {
	name, isName := nameOf(token)
	if !isName {
		return false
	}
	name = upperASCII(name)
	pragma, isPragma := strings.CutPrefix(name, "PRAGMA_")
	return name == "SQLITE_DBPAGE" || isPragma && pragma != "" && strings.Trim(pragma, pragmaNameBytes) == ""
}

const pragmaNameBytes = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_"

// nameOf is what a word, a quoted name or a string names, unquoted
//
//	"a""b" → a"b    [a b] → a b    'it''s' → it's
func nameOf(token sqlToken) (string, bool) {
	text := token.text
	switch token.kind {
	case tokenWord:
		return text, true
	case tokenString, tokenName:
		if text[0] == '[' {
			return text[1 : len(text)-1], true
		}
		quote := text[:1]
		return strings.ReplaceAll(text[1:len(text)-1], quote+quote, quote), true
	}
	return "", false
}

// clip is a token as a refusal quotes it, its first forty bytes
func clip(token string) string {
	if len(token) > 40 {
		token = token[:40] + "…"
	}
	return fmt.Sprintf("%q", token)
}

const (
	// historyRootsQuery finds the pages the migration history's table and its
	// index begin at, which a data connection may read and never write
	historyRootsQuery = `select rootpage from sqlite_schema where tbl_name = '_tinystore_migrations'`
	// rawTablesQuery shows, as its VOpen's p4, the pointer the virtual table a
	// data connection may not open has on this connection. The store's SQLite
	// has no sqlite_dbpage: a program naming it does not compile, and the first
	// line refuses its name before that.
	rawTablesQuery = `explain select 1 from pragma_optimize`
)

// explainDataSQL is the second line: it compiles the statement with EXPLAIN on
// a reader, which runs nothing, and refuses the program it gets back.
//
// A program is refused when it changes the schema, attaches or detaches a file,
// vacuums, ends a transaction, changes how the file is kept, writes the
// migration history, or opens pragma_optimize.
//
// The guarded pages are read from the same snapshot as the program.
func explainDataSQL(ctx context.Context, db *sqldb.DB, statement string, args []any) error {
	return db.View(ctx, func(tx *sqldb.Tx) error {
		guard, err := guardOf(ctx, tx)
		if err != nil {
			return err
		}
		program, err := sqldb.Query(ctx, tx, "explain "+statement, args...)
		if err != nil {
			return err
		}
		return guard.check(program)
	})
}

// guarded is what a data connection's statement may not open
type guarded struct {
	history map[int64]bool  // the migration history's root pages, in main
	tables  map[string]bool // pragma_optimize's pointer, as EXPLAIN shows it
}

func guardOf(ctx context.Context, tx *sqldb.Tx) (guarded, error) {
	guard := guarded{history: map[int64]bool{}, tables: map[string]bool{}}
	roots, err := sqldb.Query(ctx, tx, historyRootsQuery)
	if err != nil {
		return guard, err
	}
	for _, row := range roots.Values {
		guard.history[integerOf(row[0])] = true
	}
	raw, err := sqldb.Query(ctx, tx, rawTablesQuery)
	if err != nil {
		return guard, err
	}
	for _, row := range raw.Values {
		if op := opcodeOf(row); op.name == "VOpen" {
			guard.tables[op.p4] = true
		}
	}
	if len(guard.tables) != 1 {
		return guard, fmt.Errorf("server: the check found %d virtual tables where it guards one", len(guard.tables))
	}
	return guard, nil
}

var explainColumns = []string{"addr", "opcode", "p1", "p2", "p3", "p4", "p5", "comment"}

func (g guarded) check(program sqldb.Rows) error {
	if !slices.Equal(program.Columns, explainColumns) {
		return refused("a program listed in the columns %q, which are not EXPLAIN's", program.Columns)
	}
	for _, row := range program.Values {
		if why := g.refuses(opcodeOf(row)); why != "" {
			return refused("its program %s", why)
		}
	}
	return nil
}

// refusedOpcodes change the schema, vacuum, end or begin a transaction or
// change how the file is kept; a change of rows holds none of them. Attaching a
// file is a function call, which refuses checks by its name.
var refusedOpcodes = map[string]bool{
	"CreateBtree": true, "Destroy": true, "DropTable": true, "DropIndex": true, "DropTrigger": true,
	"ParseSchema": true, "SetCookie": true, "VCreate": true, "VDestroy": true, "VRename": true, "Vacuum": true,
	"IncrVacuum": true, "SqlExec": true, "JournalMode": true, "LoadAnalysis": true, "Expire": true,
	"Checkpoint": true, "AutoCommit": true, "Savepoint": true, "MaxPgcnt": true,
}

// p2IsRegister is OPFLAG_P2ISREG: an OpenWrite whose root page is computed,
// as a new index's is
const p2IsRegister = 0x10

// opcode is one row of EXPLAIN, less the addr and comment columns, which refuses does not read.
type opcode struct {
	name       string
	p1, p2, p3 int64
	p4         string
	p5         int64
}

func opcodeOf(row []any) opcode {
	return opcode{
		name: textOf(row[1]), p1: integerOf(row[2]), p2: integerOf(row[3]), p3: integerOf(row[4]),
		p4: textOf(row[5]), p5: integerOf(row[6]),
	}
}

// refuses says what op does that a data connection may not, or "" when it
// does nothing of the kind.
//
// OpenWrite's p2 and Clear's p1 are the page a table or an index begins at,
// page 1 being sqlite_schema's. OpenWrite's p3 and Clear's p2 are the database,
// 0 being main.
func (g guarded) refuses(op opcode) string {
	switch {
	case refusedOpcodes[op.name]:
		return "holds " + op.name
	case op.name == "OpenWrite" && (op.p2 == 1 || op.p5&p2IsRegister != 0 || op.p3 == 0 && g.history[op.p2]):
		return "writes the schema or the migration history"
	case op.name == "Clear" && (op.p1 == 1 || op.p2 == 0 && g.history[op.p1]):
		return "clears the migration history"
	case (op.name == "Function" || op.name == "PureFunc") &&
		(strings.HasPrefix(op.p4, "sqlite_attach(") || strings.HasPrefix(op.p4, "sqlite_detach(")):
		return "attaches or detaches a file"
	case strings.HasPrefix(op.name, "V") && g.tables[op.p4]:
		return "opens pragma_optimize"
	}
	return ""
}

func integerOf(value any) int64 {
	if n, isInteger := value.(int64); isInteger {
		return n
	}
	return 0
}

func textOf(value any) string {
	if value == nil {
		return ""
	}
	return fmt.Sprint(value)
}
