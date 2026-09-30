package server

import (
	"database/sql"
	"reflect"
	"strings"
	"testing"

	"github.com/tinyshed/tinystore/sqldb"
)

var tokenKinds = [...]string{"space", "word", "name", "string", "semi", "other", "illegal"}

func describeTokens(tokens []sqlToken) string {
	described := make([]string, len(tokens))
	for i, token := range tokens {
		described[i] = tokenKinds[token.kind] + ":" + token.text
	}
	return strings.Join(described, " ")
}

// the check reads SQL a token at a time as SQLite's tokenizer does
func TestTheCheckReadsSQLitesTokens(t *testing.T) {
	for _, c := range []struct{ sql, tokens string }{
		{`select 'it''s';`, `word:select string:'it''s' semi:;`},
		{"select \"a\"\"b\", [c d], `e``f`", "word:select name:\"a\"\"b\" other:, name:[c d] other:, name:`e``f`"},
		{"select 1 -- x\r; y\nfrom t", `word:select other:1 word:from word:t`},
		{`select /**/ 1 /* a */`, `word:select other:1`},
		{`select /*/ 1`, `word:select illegal:/*/ 1`},
		{`select $a(;') ;`, `word:select other:$a(;') semi:;`},
		{
			`select $a::b, :c, @d, #e, ?1, ?`,
			`word:select other:$a::b other:, other::c other:, other:@d other:, other:#e other:, other:?1 other:, other:?`,
		},
		{`select @, $a(b c)`, `word:select illegal:@ other:, illegal:$a(b word:c other:)`},
		{
			`select x'0aff', X'', x'1', x'zz;'`,
			`word:select other:x'0aff' other:, other:X'' other:, illegal:x'1' other:, illegal:x'zz;'`,
		},
		{
			`select 0x1f_00, 1_000.5e-3, .5, 12abc, 1e--2`,
			`word:select other:0x1f_00 other:, other:1_000.5e-3 other:, other:.5 other:, illegal:12abc other:, illegal:1e`,
		},
		{"\xef\xbb\xbfselect 1", `word:select other:1`},
		{"select\xc2\xa01", "word:select\xc2\xa01"},
		{"select\v1", "word:select illegal:\v other:1"},
		{" \vselect", `word:select`},
		{`a->>'$.b'`, `word:a other:- other:> other:> string:'$.b'`},
		{`select a!=b, !a`, `word:select word:a other:!= word:b other:, illegal:! word:a`},
		{`select [a]]`, `word:select name:[a] illegal:]`},
		{"a$b _c \xc3\xbf", "word:a$b word:_c word:\xc3\xbf"},
		{`select 'x`, `word:select illegal:'x`},
		{`select [x`, `word:select illegal:[x`},
		{`/*`, `other:/ other:*`},
		{`--`, ``},
	} {
		if got := describeTokens(tokensOf(c.sql)); got != c.tokens {
			t.Errorf("%q read as\n\t%s\nwant\n\t%s", c.sql, got, c.tokens)
		}
	}
}

// SQLite ends a statement where the check does: what the check reads as one
// statement runs as one, and what it reads as two SQLite refuses to prepare as
// one. Each case would answer otherwise if SQLite read it otherwise.
func TestSQLiteEndsAStatementWhereTheCheckDoes(t *testing.T) {
	db, _ := openScratch(t)
	for _, c := range []struct {
		name string
		sql  string
		args []any
		one  bool // the check reads it as one statement
		rows [][]any
	}{
		{
			"a Tcl parameter's index holds a ;", `select $a(;)`,
			[]any{sql.Named("a(;)", int64(7))},
			true,
			[][]any{{int64(7)}},
		},
		{"a line comment runs past a carriage return", "select 1 -- ;\r; select 2", nil, true, [][]any{{int64(1)}}},
		{"a newline ends a line comment", "select 1 -- ;\n; select 2", nil, false, nil},
		{"comments do not nest", `select 1 /* /* */ , 2`, nil, true, [][]any{{int64(1), int64(2)}}},
		{"a bracketed name holds a ;", `select [a;b] from (select 1 as [a;b])`, nil, true, [][]any{{int64(1)}}},
		{"a byte order mark is whitespace", "\xef\xbb\xbfselect 1", nil, true, [][]any{{int64(1)}}},
		{"a blob literal", `select x'00ff'`, nil, true, [][]any{{[]byte{0, 0xff}}}},
		{"a doubled quote stands for itself", `select 'a'';''b'`, nil, true, [][]any{{"a';'b"}}},
	} {
		if lexed := readDataSQL(c.sql); (lexed == nil) != c.one {
			t.Errorf("%s: the check says %v", c.name, lexed)
		}
		rows, err := sqldb.Query(t.Context(), db, c.sql, c.args...)
		switch {
		case !c.one && (err == nil || !strings.Contains(err.Error(), "multiple statements")):
			t.Errorf("%s: SQLite reads it as one statement: %#v, %v", c.name, rows.Values, err)
		case c.one && (err != nil || !reflect.DeepEqual(rows.Values, c.rows)):
			t.Errorf("%s: SQLite answers %#v, %v", c.name, rows.Values, err)
		}
	}
}
