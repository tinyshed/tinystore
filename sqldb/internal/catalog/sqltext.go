package catalog

import (
	"encoding/hex"
	"math"
	"strconv"
	"strings"
)

// tokenKind is what a token of SQL text is; a lexer, not a parser, is all the
// comparison of expressions needs
type tokenKind int

const (
	word       tokenKind = iota + 1 // a keyword or a bare name
	identifier                      // a name in "", ``, or []
	text                            // a string literal
	number
	blob
	punctuation
	space // whitespace and comments
)

type token struct {
	kind tokenKind
	text string
}

// tokenize splits SQL into tokens; what it cannot read goes as punctuation
func tokenize(sql string) []token {
	var tokens []token
	for i := 0; i < len(sql); {
		kind, end := next(sql, i)
		tokens = append(tokens, token{kind: kind, text: sql[i:end]})
		i = end
	}
	return tokens
}

func next(sql string, i int) (tokenKind, int) {
	c := sql[i]
	switch {
	case c == ' ' || c == '\t' || c == '\n' || c == '\r':
		return space, i + 1
	case strings.HasPrefix(sql[i:], "--"):
		return space, lineEnd(sql, i)
	case strings.HasPrefix(sql[i:], "/*"):
		return space, commentEnd(sql, i)
	case c == '\'':
		return text, quoted(sql, i, '\'')
	case c == '"' || c == '`':
		return identifier, quoted(sql, i, c)
	case c == '[':
		return identifier, closing(sql, i, ']')
	case (c == 'x' || c == 'X') && i+1 < len(sql) && sql[i+1] == '\'':
		return blob, quoted(sql, i+1, '\'')
	case isDigit(c) || (c == '.' && i+1 < len(sql) && isDigit(sql[i+1])):
		return number, numberEnd(sql, i)
	case isWordStart(c):
		end := i + 1
		for end < len(sql) && isWordPart(sql[end]) {
			end++
		}
		return word, end
	}
	return punctuation, i + 1
}

func lineEnd(sql string, i int) int {
	if end := strings.IndexByte(sql[i:], '\n'); end >= 0 {
		return i + end + 1
	}
	return len(sql)
}

func commentEnd(sql string, i int) int {
	if end := strings.Index(sql[i+2:], "*/"); end >= 0 {
		return i + 2 + end + 2
	}
	return len(sql)
}

// quoted is where a quote opened at i closes, a doubled quote standing for one
func quoted(sql string, i int, quote byte) int {
	for end := i + 1; end < len(sql); end++ {
		if sql[end] != quote {
			continue
		}
		if end+1 < len(sql) && sql[end+1] == quote {
			end++
			continue
		}
		return end + 1
	}
	return len(sql)
}

func closing(sql string, i int, end byte) int {
	if at := strings.IndexByte(sql[i:], end); at >= 0 {
		return i + at + 1
	}
	return len(sql)
}

func numberEnd(sql string, i int) int {
	end := i
	if strings.HasPrefix(strings.ToLower(sql[i:]), "0x") {
		end += 2
		for end < len(sql) && isHex(sql[end]) {
			end++
		}
		return end
	}
	for end < len(sql) && (isDigit(sql[end]) || sql[end] == '.' || sql[end] == '_') {
		end++
	}
	if end < len(sql) && (sql[end] == 'e' || sql[end] == 'E') {
		end++
		if end < len(sql) && (sql[end] == '+' || sql[end] == '-') {
			end++
		}
		for end < len(sql) && isDigit(sql[end]) {
			end++
		}
	}
	return end
}

func isDigit(c byte) bool { return c >= '0' && c <= '9' }
func isHex(c byte) bool   { return isDigit(c) || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F') }

func isWordStart(c byte) bool {
	return c == '_' || c == '$' || c >= 0x80 || (c|0x20 >= 'a' && c|0x20 <= 'z')
}
func isWordPart(c byte) bool { return isWordStart(c) || isDigit(c) }

func solid(tokens []token) []token {
	kept := tokens[:0:0]
	for _, t := range tokens {
		if t.kind != space {
			kept = append(kept, t)
		}
	}
	return kept
}

// Fold is an expression with its space and comments taken out and its words
// and names in lower case, outside quotes, so that two spellings of one rule
// compare equal:
//
//	CHECK ( done IN(0,1) ) → check(donein(0,1))    check (done in (0, 1)) → check(donein(0,1))
func Fold(expression string) string {
	var out strings.Builder
	for _, t := range solid(tokenize(expression)) {
		switch t.kind {
		case word:
			out.WriteString(strings.ToLower(t.text))
		case identifier:
			out.WriteString(strings.ToLower(unquote(t.text)))
		default:
			out.WriteString(t.text)
		}
	}
	return out.String()
}

// unquote is a quoted name as SQLite reads it: "a""b" → a"b, [a b] → a b
func unquote(name string) string {
	if len(name) < 2 {
		return name
	}
	switch name[0] {
	case '"', '`':
		return strings.ReplaceAll(name[1:len(name)-1], name[:1]+name[:1], name[:1])
	case '[':
		return name[1 : len(name)-1]
	}
	return name
}

// literal is the value a default's text spells when it is a literal rather
// than an expression, as SQLite would compute it:
//
//	0 → 0    00 → 0    -1.5 → -1.5    'it''s' → "it's"    X'0102' → [1 2]    NULL → nil
func literal(textOf string) (value any, isLiteral bool) {
	tokens := solid(tokenize(textOf))
	negative := false
	if len(tokens) == 2 && tokens[0].kind == punctuation && (tokens[0].text == "-" || tokens[0].text == "+") {
		negative, tokens = tokens[0].text == "-", tokens[1:]
		if tokens[0].kind != number {
			return nil, false
		}
	}
	if len(tokens) != 1 {
		return nil, false
	}
	t := tokens[0]
	switch t.kind {
	case number:
		return numberOf(t.text, negative)
	case text:
		return strings.ReplaceAll(t.text[1:len(t.text)-1], "''", "'"), true
	case blob:
		decoded, err := hex.DecodeString(t.text[2 : len(t.text)-1])
		return decoded, err == nil
	case word:
		switch strings.ToUpper(t.text) {
		case "NULL":
			return nil, true
		case "TRUE":
			return int64(1), true
		case "FALSE":
			return int64(0), true
		}
	default:
	}
	return nil, false
}

// twosComplement is how SQLite reads a hexadecimal literal: 0xffffffffffffffff is -1
func twosComplement(n uint64) int64 {
	return int64(n) //nolint:gosec // the wrap is the meaning
}

func numberOf(digits string, negative bool) (any, bool) {
	digits = strings.ReplaceAll(digits, "_", "")
	if strings.HasPrefix(strings.ToLower(digits), "0x") {
		n, err := strconv.ParseUint(digits[2:], 16, 64)
		if err != nil {
			return nil, false
		}
		if negative {
			return -twosComplement(n), true
		}
		return twosComplement(n), true
	}
	if n, err := strconv.ParseInt(digits, 10, 64); err == nil {
		if negative {
			return -n, true
		}
		return n, true
	}
	f, err := strconv.ParseFloat(digits, 64)
	if err != nil && !math.IsInf(f, 0) {
		return nil, false
	}
	if negative {
		return -f, true
	}
	return f, true
}

// element is one comma-separated part of a CREATE TABLE's parentheses: a
// column's definition, or a constraint of the table
type element struct {
	text   string
	name   string // the column's, "" for a constraint
	checks []string
}

// elements splits a CREATE TABLE's text into the definitions between its
// outer parentheses, with the CHECK expressions each holds
func elements(create string) []element {
	tokens := tokenize(create)
	start := 0
	for start < len(tokens) && tokens[start].text != "(" {
		start++
	}
	var found []element
	depth, from := 0, start+1
	for i := start; i < len(tokens); i++ {
		switch tokens[i].text {
		case "(":
			depth++
		case ")":
			depth--
			if depth == 0 {
				return append(found, elementOf(tokens[from:i]))
			}
		case ",":
			if depth == 1 {
				found = append(found, elementOf(tokens[from:i]))
				from = i + 1
			}
		}
	}
	return found
}

var tableConstraints = map[string]bool{
	"CONSTRAINT": true, "PRIMARY": true, "UNIQUE": true, "CHECK": true, "FOREIGN": true,
}

func elementOf(tokens []token) element {
	var out strings.Builder
	for _, t := range tokens {
		out.WriteString(t.text)
	}
	e := element{text: strings.TrimSpace(out.String()), checks: checksIn(tokens)}
	if kept := solid(tokens); len(kept) > 0 && !tableConstraints[strings.ToUpper(kept[0].text)] {
		e.name = unquote(kept[0].text)
	}
	return e
}

// checksIn is the text inside each CHECK ( … ) of tokens
func checksIn(tokens []token) []string {
	var checks []string
	for i := 0; i < len(tokens); i++ {
		if tokens[i].kind != word || !strings.EqualFold(tokens[i].text, "CHECK") {
			continue
		}
		open := i + 1
		for open < len(tokens) && tokens[open].kind == space {
			open++
		}
		if open == len(tokens) || tokens[open].text != "(" {
			continue
		}
		depth := 0
		var inside strings.Builder
		for j := open; j < len(tokens); j++ {
			switch tokens[j].text {
			case "(":
				depth++
			case ")":
				depth--
			}
			if depth == 0 {
				checks = append(checks, strings.TrimSpace(inside.String()))
				i = j
				break
			}
			if j > open {
				inside.WriteString(tokens[j].text)
			}
		}
	}
	return checks
}
