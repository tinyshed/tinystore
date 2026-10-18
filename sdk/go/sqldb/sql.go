package sqldb

import (
	"encoding/json"
	"fmt"
	"regexp"
	"sort"
	"strings"

	"tinyshed.org/tinystore/internal/core"
)

// Piece is a piece of SQL and the values of its ?s, which are always values
// and never SQL: SQL makes one, and a call, a batch and a table's query take it.
type Piece struct {
	text   string
	values []any
	// bare says it stands beside others in an and or an or without
	// parentheses: one column's comparison
	bare bool
	err  error
}

// Statement is a statement of a batch: a piece of SQL.
type Statement = Piece

// Condition is a condition a query keeps to: equal values by column, Eq, or
// a piece of SQL.
type Condition interface{ condition() Piece }

func (p Piece) condition() Piece { return p }

// Text is the piece's SQL, and Values the values of its ?s.
func (p Piece) Text() string { return p.text }

// Values are the values of the piece's ?s, as they leave.
func (p Piece) Values() []any { return p.values }

// Eq is equal values by column, written in the order of the columns' names:
// nil is is null, and a slice in (...).
type Eq map[string]any

func (e Eq) condition() Piece {
	names := make([]string, 0, len(e))
	for name := range e {
		names = append(names, name)
	}
	sort.Strings(names)
	pieces := make([]Piece, len(names))
	for i, name := range names {
		pieces[i] = equal(name, e[name])
	}
	if len(pieces) == 1 {
		return pieces[0]
	}
	return group(pieces, "and", "1")
}

// embedder is something that writes itself as SQL inside another piece: a
// table's query, in parentheses.
type embedder interface{ embedded() Piece }

// SQL is a piece of SQL from text and the values of its ?s; a piece or a
// query in a ?'s place is written in: sqldb.SQL("exists ?", paid).
func SQL(text string, values ...any) Piece {
	counted := placeholdersOf(text)
	if len(counted.named) > 0 || counted.numbered {
		return Piece{err: core.Errorf(core.Invalid, "a piece of SQL takes its values at ? alone: %s", text)}
	}
	if counted.positional != len(values) {
		return Piece{err: core.Errorf(core.Invalid, "%d placeholders and %d values: each ? takes one value", counted.positional, len(values))}
	}
	parts := splitAtPlaceholders(text, counted.positional)
	out := Piece{text: parts[0]}
	for i, value := range values {
		var inner *Piece
		switch v := value.(type) {
		case Piece:
			inner = &v
		case embedder:
			embedded := v.embedded()
			inner = &embedded
		}
		if inner == nil {
			cell, err := cellOf(value, fmt.Sprintf("value %d", i+1))
			if err != nil {
				return Piece{err: err}
			}
			out.text += "?"
			out.values = append(out.values, cell)
		} else {
			if inner.err != nil {
				return *inner
			}
			out.text += inner.text
			out.values = append(out.values, inner.values...)
		}
		out.text += parts[i+1]
	}
	return out
}

// Or is conditions joined with or, in parentheses; an empty or finds nothing.
func Or(conditions ...Condition) Piece { return groupOf(conditions, "or", "0") }

// And is conditions joined with and, in parentheses; an empty and finds everything.
func And(conditions ...Condition) Piece { return groupOf(conditions, "and", "1") }

// List is a list for in (...), as one value SQLite reads with json_each: any
// length, the same statement.
func List[T any](values []T) Piece {
	items := make([]any, len(values))
	for i, v := range values {
		item, err := listed(v)
		if err != nil {
			return Piece{err: err}
		}
		items[i] = item
	}
	text, err := json.Marshal(items)
	if err != nil {
		return Piece{err: core.Wrap(core.Invalid, err, "a list JSON cannot write")}
	}
	return Piece{text: "(select value from json_each(?))", values: []any{string(text)}}
}

// JSON is a value kept as its JSON.
func JSON(value any) Piece {
	text, err := json.Marshal(value)
	if err != nil {
		return Piece{err: core.Wrap(core.Invalid, err, "a value JSON cannot write")}
	}
	return Piece{text: "?", values: []any{string(text)}}
}

// Ident is a name, quoted, never SQL: a column a request chooses.
func Ident(name string) Piece {
	quoted, err := identOf(name)
	return Piece{text: quoted, err: err}
}

// Has is a JSON list in column name holding value.
func Has(name string, value any) Piece {
	column, err := identOf(name)
	if err != nil {
		return Piece{err: err}
	}
	cell, err := cellOf(value, name+"'s value")
	if err != nil {
		return Piece{err: err}
	}
	return Piece{text: "exists (select 1 from json_each(" + column + ") where value = ?)", values: []any{cell}}
}

func listed(value any) (any, error) {
	switch v := value.(type) {
	case nil, bool, string, float64, float32, int, int8, int16, int32, int64, uint, uint8, uint16, uint32, uint64:
		return v, nil
	case []byte:
		return nil, core.Errorf(core.Invalid, "a list for in (...) holds text, numbers, booleans and nil")
	}
	return cellOf(value, "a list's item")
}

func equal(name string, value any) Piece {
	column, err := identOf(name)
	if err != nil {
		return Piece{err: err}
	}
	if value == nil {
		return Piece{text: column + " is null", bare: true}
	}
	if items, ok := sliceOf(value); ok {
		listedPiece := List(items)
		return Piece{text: column + " in " + listedPiece.text, values: listedPiece.values, bare: true, err: listedPiece.err}
	}
	cell, err := cellOf(value, name)
	if err != nil {
		return Piece{err: err}
	}
	return Piece{text: column + " = ?", values: []any{cell}, bare: true}
}

func groupOf(conditions []Condition, joiner, empty string) Piece {
	pieces := make([]Piece, len(conditions))
	for i, c := range conditions {
		pieces[i] = c.condition()
	}
	return group(pieces, joiner, empty)
}

func group(pieces []Piece, joiner, empty string) Piece {
	if len(pieces) == 0 {
		return Piece{text: empty}
	}
	joined := joinedWith(pieces, joiner)
	if joined.err != nil {
		return joined
	}
	return Piece{text: "(" + joined.text + ")", values: joined.values}
}

// joinedWith is pieces joined with and or or, each in parentheses unless it
// may stand as it is.
func joinedWith(pieces []Piece, joiner string) Piece {
	several := len(pieces) > 1
	texts := make([]string, len(pieces))
	var values []any
	for i, each := range pieces {
		if each.err != nil {
			return each
		}
		if several && !each.bare && !standsAlone(each.text) {
			texts[i] = "(" + each.text + ")"
		} else {
			texts[i] = each.text
		}
		values = append(values, each.values...)
	}
	return Piece{text: strings.Join(texts, " "+joiner+" "), values: values}
}

// grouped says whether text is one group in parentheses, or a bare 0 or 1,
// which may stand beside others as it is.
func grouped(text string) bool {
	if !strings.HasPrefix(text, "(") || !strings.HasSuffix(text, ")") {
		return text == "0" || text == "1"
	}
	depth := 0
	for at, c := range text {
		switch c {
		case '(':
			depth++
		case ')':
			depth--
		}
		if depth == 0 && at < len(text)-1 {
			return false
		}
	}
	return true
}

var word = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_$]*`)

// standsAlone says whether a piece keeps its meaning beside others in an and
// or an or: one group, or text with no and or or of its own outside quotes,
// comments and parentheses, which would bind with its neighbours'.
func standsAlone(text string) bool {
	if grouped(text) {
		return true
	}
	depth := 0
	for at := 0; at < len(text); at++ {
		c := text[at]
		switch {
		case c == '\'' || c == '"' || c == '`':
			at = closing(text, at, c)
		case strings.HasPrefix(text[at:], "--"):
			end := strings.IndexByte(text[at:], '\n')
			if end < 0 {
				return true
			}
			at += end
		case strings.HasPrefix(text[at:], "/*"):
			end := strings.Index(text[at+2:], "*/")
			if end < 0 {
				return true
			}
			at += end + 3
		case c == '(':
			depth++
		case c == ')':
			depth--
		case depth == 0 && isAlpha(c) && (at == 0 || !wordChar(text[at-1])):
			w := word.FindString(text[at:])
			if lower := strings.ToLower(w); lower == "and" || lower == "or" {
				return false
			}
			at += len(w) - 1
		}
	}
	return true
}

func isAlpha(c byte) bool  { return c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' }
func wordChar(c byte) bool { return isAlpha(c) || c >= '0' && c <= '9' || c == '_' || c == '$' }

// closing is the index of the quote that ends a quoted run starting at at, a
// doubled quote inside it kept.
func closing(text string, at int, quote byte) int {
	for next := at + 1; next < len(text); next++ {
		if text[next] == quote {
			if next+1 < len(text) && text[next+1] == quote {
				next++
				continue
			}
			return next
		}
	}
	return len(text)
}

type placeholders struct {
	positional int
	named      []string
	numbered   bool
}

// placeholdersOf is what a text's placeholders are, counted outside quotes and comments.
func placeholdersOf(text string) placeholders {
	var p placeholders
	for at := 0; at < len(text); at++ {
		c := text[at]
		var next byte
		if at+1 < len(text) {
			next = text[at+1]
		}
		switch {
		case c == '\'' || c == '"' || c == '`':
			at = closing(text, at, c)
		case c == '[':
			end := strings.IndexByte(text[at+1:], ']')
			if end < 0 {
				return p
			}
			at += end + 1
		case c == '-' && next == '-':
			end := strings.IndexByte(text[at:], '\n')
			if end < 0 {
				return p
			}
			at += end
		case c == '/' && next == '*':
			end := strings.Index(text[at+2:], "*/")
			if end < 0 {
				return p
			}
			at += end + 3
		case c == '?':
			if next >= '0' && next <= '9' {
				p.numbered = true
			} else {
				p.positional++
			}
		case (c == ':' || c == '@' || c == '$') && (isAlpha(next) || next == '_'):
			w := word.FindString(text[at+1:])
			found := false
			for _, n := range p.named {
				found = found || n == w
			}
			if !found {
				p.named = append(p.named, w)
			}
			at += len(w)
		}
	}
	return p
}

// splitAtPlaceholders is text cut at its ?s outside quotes and comments, count + 1 parts.
func splitAtPlaceholders(text string, count int) []string {
	parts := make([]string, 0, count+1)
	start := 0
	for at := 0; at < len(text); at++ {
		c := text[at]
		var next byte
		if at+1 < len(text) {
			next = text[at+1]
		}
		switch {
		case c == '\'' || c == '"' || c == '`':
			at = closing(text, at, c)
		case c == '[':
			if end := strings.IndexByte(text[at+1:], ']'); end >= 0 {
				at += end + 1
			}
		case c == '-' && next == '-':
			if end := strings.IndexByte(text[at:], '\n'); end < 0 {
				at = len(text)
			} else {
				at += end
			}
		case c == '/' && next == '*':
			if end := strings.Index(text[at+2:], "*/"); end < 0 {
				at = len(text)
			} else {
				at += end + 3
			}
		case c == '?':
			parts = append(parts, text[start:at])
			start = at + 1
		}
	}
	return append(parts, text[start:])
}

// identOf is a name as SQL quotes it: "u"."created_at", each part on its own.
func identOf(name string) (string, error) {
	if name == "" || strings.ContainsRune(name, 0) {
		return "", core.Errorf(core.Invalid, "the name %q: a name is text without NUL", name)
	}
	parts := strings.Split(name, ".")
	for i, part := range parts {
		parts[i] = `"` + strings.ReplaceAll(part, `"`, `""`) + `"`
	}
	return strings.Join(parts, "."), nil
}

func mustIdent(name string) string {
	quoted, _ := identOf(name)
	return quoted
}
