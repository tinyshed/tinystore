package server

import (
	"bufio"
	"bytes"
	"crypto/subtle"
	"encoding/base64"
	"fmt"
	"strings"

	"github.com/tinyshed/tinystore/server/wire"
)

// Tokens let remote connections in: a HELLO's token finds its capability.
type Tokens struct {
	known []tokenLine
}

// tokenLine is one token of a tokens file, with the capability it gives
type tokenLine struct {
	secret     []byte
	capability wire.Capability
}

// the bytes a token holds, written in base64url without padding
const tokenBytes = 32

// ParseTokens reads a tokens file, a token a line with its capability first:
//
//	admin q1q0l3yZ3JvYw8g7oM2sYAq1q0l3yZ3JvYw8g7oM2sY
//	data  Zm9vYmFyYmF6cXV4Zm9vYmFyYmF6cXV4Zm9vYmFyYmE
//
// A blank line, or one that begins with #, says nothing.
func ParseTokens(text []byte) (Tokens, error) {
	var tokens Tokens
	lines := bufio.NewScanner(bytes.NewReader(text))
	for n := 1; lines.Scan(); n++ {
		line := strings.TrimSpace(lines.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parsed, err := parseToken(line)
		if err != nil {
			return Tokens{}, fmt.Errorf("server: tokens, line %d: %w", n, err)
		}
		tokens.known = append(tokens.known, parsed)
	}
	return tokens, lines.Err()
}

func parseToken(line string) (tokenLine, error) {
	fields := strings.Fields(line)
	if len(fields) != 2 {
		return tokenLine{}, fmt.Errorf("a line is a capability and a token, not %d fields", len(fields))
	}
	capability := wire.Capability(fields[0])
	if capability != wire.Admin && capability != wire.Data {
		return tokenLine{}, fmt.Errorf("a capability is admin or data, not %q", fields[0])
	}
	secret, err := base64.RawURLEncoding.DecodeString(fields[1])
	if err != nil || len(secret) != tokenBytes {
		return tokenLine{}, fmt.Errorf("a token is %d random bytes in base64url", tokenBytes)
	}
	return tokenLine{secret: []byte(fields[1]), capability: capability}, nil
}

// capability is what given lets a connection do; every token is compared,
// each in constant time, so that the time taken says nothing of which matched
func (t Tokens) capability(given string) (wire.Capability, bool) {
	var found wire.Capability
	for _, known := range t.known {
		if subtle.ConstantTimeCompare(known.secret, []byte(given)) == 1 {
			found = known.capability
		}
	}
	return found, found != ""
}
