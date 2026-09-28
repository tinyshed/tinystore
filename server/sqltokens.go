package server

import "strings"

// tokenKind is what one of SQLite's tokens is to a data connection's check,
// which tells apart only what decides where a statement ends and which names
// it uses
type tokenKind uint8

const (
	tokenSpace   tokenKind = iota // whitespace, or a comment
	tokenWord                     // a keyword or a bare name
	tokenName                     // a quoted name: "…", `…` or […]
	tokenString                   // '…', which SQLite also takes for a name where one may stand
	tokenSemi                     // ;
	tokenOther                    // an operator, a number, a parameter or a blob
	tokenIllegal                  // what SQLite refuses, or what runs unclosed to the end
)

type sqlToken struct {
	kind tokenKind
	text string
}

// tokensOf is statement's tokens as SQLite reads them, but its whitespace and
// comments
func tokensOf(statement string) []sqlToken {
	var tokens []sqlToken
	for rest := statement; rest != ""; {
		kind, n := nextToken(rest)
		if kind != tokenSpace {
			tokens = append(tokens, sqlToken{kind: kind, text: rest[:n]})
		}
		rest = rest[n:]
	}
	return tokens
}

// nextToken reads the token sql begins with as sqlite3GetToken in SQLite's
// tokenize.c reads it, and says its length; sql is not empty. It departs from
// SQLite once: a comment left open, which SQLite takes to run to the end, is
// illegal here, since the check sees no end to it.
func nextToken(sql string) (tokenKind, int) {
	switch c := sql[0]; {
	case c == ' ', c == '\t', c == '\n', c == '\f', c == '\r':
		return tokenSpace, 1 + spaceRun(sql[1:])
	case c == '-' && at(sql, 1) == '-':
		return tokenSpace, lineComment(sql)
	case c == '/' && at(sql, 1) == '*' && len(sql) > 2:
		return blockComment(sql)
	case c == ';':
		return tokenSemi, 1
	case c == '\'', c == '"', c == '`':
		return quoted(sql)
	case c == '[':
		return bracketed(sql)
	case c == '?':
		return tokenOther, 1 + digitRun(sql[1:], false)
	case c == '$', c == '@', c == ':', c == '#':
		return parameter(sql)
	case (c == 'x' || c == 'X') && at(sql, 1) == '\'':
		return blob(sql)
	case isDigit(c), c == '.' && isDigit(at(sql, 1)):
		return number(sql)
	case c == 0xef && at(sql, 1) == 0xbb && at(sql, 2) == 0xbf:
		return tokenSpace, 3 // the UTF-8 byte order mark
	case isWordStart(c):
		return tokenWord, 1 + idRun(sql[1:])
	case c == '!' && at(sql, 1) == '=':
		return tokenOther, 2
	case strings.IndexByte("()+*%,&~.=<>|-/", c) >= 0:
		return tokenOther, 1
	}
	return tokenIllegal, 1
}

// at is the byte at i, or 0 past the end, where SQLite reads its terminator
func at(sql string, i int) byte {
	if i < len(sql) {
		return sql[i]
	}
	return 0
}

// lineComment runs from -- to the newline, which it leaves; a carriage return
// does not end it
func lineComment(sql string) int {
	if end := strings.IndexByte(sql[2:], '\n'); end >= 0 {
		return 2 + end
	}
	return len(sql)
}

// blockComment runs from /* to the first */, which may begin at its third
// byte: comments do not nest
//
//	/**/ → 4    /*/ → open to the end
func blockComment(sql string) (tokenKind, int) {
	end := strings.Index(sql[2:], "*/")
	if end < 0 {
		return tokenIllegal, len(sql)
	}
	return tokenSpace, 2 + end + 2
}

// quoted reads '…' as a string, and "…" and `…` as names; its delimiter
// twice stands for itself
func quoted(sql string) (tokenKind, int) {
	delimiter := sql[0]
	for i := 1; i < len(sql); i++ {
		switch {
		case sql[i] != delimiter:
		case at(sql, i+1) == delimiter:
			i++
		case delimiter == '\'':
			return tokenString, i + 1
		default:
			return tokenName, i + 1
		}
	}
	return tokenIllegal, len(sql)
}

// bracketed reads […], which ends at its first ] and escapes nothing
func bracketed(sql string) (tokenKind, int) {
	end := strings.IndexByte(sql, ']')
	if end < 0 {
		return tokenIllegal, len(sql)
	}
	return tokenName, end + 1
}

// parameter reads $name, @name, :name or #name, whose name may go on past a ::
// and end in a Tcl array's index, which runs to a ) and takes anything but
// whitespace, a ; and quotes included:
//
//	$a(;')  → one parameter    $a::b → one parameter    @ → illegal
func parameter(sql string) (tokenKind, int) {
	named := 0
	i := 1
	for ; i < len(sql); i++ {
		c := sql[i]
		switch {
		case isIDChar(c):
			named++
			continue
		case c == '(' && named > 0:
			return tclIndex(sql, i)
		case c == ':' && at(sql, i+1) == ':':
			i++
			continue
		}
		break
	}
	if named == 0 {
		return tokenIllegal, i
	}
	return tokenOther, i
}

// tclIndex reads a parameter's index from its ( to its )
func tclIndex(sql string, open int) (tokenKind, int) {
	i := open + 1
	for i < len(sql) && !isSpace(sql[i]) && sql[i] != ')' {
		i++
	}
	if at(sql, i) != ')' {
		return tokenIllegal, i
	}
	return tokenOther, i + 1
}

// blob reads x'…', an even number of hex digits; anything else is illegal up
// to the next quote, which it takes
func blob(sql string) (tokenKind, int) {
	i := 2
	for isHex(at(sql, i)) {
		i++
	}
	kind := tokenOther
	if at(sql, i) != '\'' || i%2 == 1 {
		kind = tokenIllegal
		for i < len(sql) && sql[i] != '\'' {
			i++
		}
	}
	if i < len(sql) {
		i++
	}
	return kind, i
}

// number reads a numeric literal: hexadecimal, or digits with a fraction and
// an exponent, any of them with _ between digits; a letter after it makes it
// illegal, as in 12abc
func number(sql string) (tokenKind, int) {
	var i int
	if sql[0] == '0' && (at(sql, 1) == 'x' || at(sql, 1) == 'X') && isHex(at(sql, 2)) {
		i = 2 + digitRun(sql[2:], true)
	} else {
		i = digitRun(sql, false)
		if at(sql, i) == '.' {
			i += 1 + digitRun(sql[i+1:], false)
		}
		if exponent(sql, i) {
			i += 2 + digitRun(sql[i+2:], false)
		}
	}
	if letters := idRun(sql[i:]); letters > 0 {
		return tokenIllegal, i + letters
	}
	return tokenOther, i
}

// exponent says an exponent begins at i: an e, then a digit, or a sign and a
// digit
func exponent(sql string, i int) bool {
	if e := at(sql, i); e != 'e' && e != 'E' {
		return false
	}
	next := at(sql, i+1)
	return isDigit(next) || (next == '+' || next == '-') && isDigit(at(sql, i+2))
}

func digitRun(sql string, hex bool) int {
	i := 0
	for i < len(sql) && (isDigit(sql[i]) || sql[i] == '_' || hex && isHex(sql[i])) {
		i++
	}
	return i
}

func spaceRun(sql string) int {
	i := 0
	for i < len(sql) && isSpace(sql[i]) {
		i++
	}
	return i
}

func idRun(sql string) int {
	i := 0
	for i < len(sql) && isIDChar(sql[i]) {
		i++
	}
	return i
}

// isSpace is sqlite3Isspace, which a vertical tab passes, though no token
// begins with one
func isSpace(c byte) bool {
	return c == ' ' || c >= '\t' && c <= '\r'
}

func isDigit(c byte) bool { return c >= '0' && c <= '9' }

func isHex(c byte) bool { return isDigit(c) || c >= 'a' && c <= 'f' || c >= 'A' && c <= 'F' }

func isLetter(c byte) bool { return c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' }

// isWordStart is a byte a keyword or a bare name begins with: every byte of a
// multibyte character is one, so SQLite reads no Unicode whitespace
func isWordStart(c byte) bool { return isLetter(c) || c == '_' || c >= 0x80 }

// isIDChar is a byte a name goes on with: a digit and a $ too
func isIDChar(c byte) bool { return isWordStart(c) || isDigit(c) || c == '$' }

// upperASCII folds only ASCII letters, as SQLite compares keywords and names:
// Unicode folding would read ſelect as SELECT
func upperASCII(s string) string {
	upper := []byte(s)
	for i, c := range upper {
		if c >= 'a' && c <= 'z' {
			upper[i] = c - 'a' + 'A'
		}
	}
	return string(upper)
}
