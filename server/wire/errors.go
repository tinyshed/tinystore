package wire

import (
	"maps"
	"slices"
	"strings"
	"unicode/utf8"
)

// Code is what failed, as a stream's Error or a GoAway says it.
type Code string

// The first nine are the store's sentinels, so that a client's error means
// what errors.Is means in Go.
const (
	CodeInvalid        Code = "invalid"         // the request cannot be done as asked
	CodeLimit          Code = "limit"           // a bound: memory, size or count
	CodeClosed         Code = "closed"          // the store or the handle closed
	CodeInUse          Code = "in_use"          // a name is taken
	CodeConflict       Code = "conflict"        // a condition or a version no longer holds
	CodeCorrupt        Code = "corrupt"         // stored bytes no longer read
	CodeTooOld         Code = "too_old"         // a time before its engine's window
	CodeTooNew         Code = "too_new"         // a time past its engine's window
	CodeSuspended      Code = "suspended"       // a series in quarantine
	CodeOutcomeUnknown Code = "outcome_unknown" // a commit whose result is unknown
	CodePermission     Code = "permission"      // the connection's capability does not allow it
	CodeUnimplemented  Code = "unimplemented"   // a method this server does not have
	CodeCancelled      Code = "cancelled"       // the client cancelled it
	CodeUnavailable    Code = "unavailable"     // the server is closing: send it on another connection
	CodeInternal       Code = "internal"        // a fault of the server's

	// a GoAway's alone
	CodeProtocol        Code = "protocol"        // a frame the server cannot take
	CodeUnauthenticated Code = "unauthenticated" // a HELLO without the token the connection needs
)

// Error is a stream's failure, the body of its final frame with FlagError.
type Error struct {
	Code    Code
	Message string // what failed, as the engine's error says it
	// What names the item the failure is about: a bucket and key, a queue
	// and key, a series' labels, a table and constraint.
	What map[string]string
}

func (e *Error) Error() string {
	if e.Message == "" {
		return string(e.Code)
	}
	return string(e.Code) + ": " + e.Message
}

// Append writes the error with What's names in their byte order. A name's
// value that is not UTF-8, as a key of bytes may be, travels as bin.
func (e *Error) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Str(1, string(e.Code))
	m.Str(2, strings.ToValidUTF8(e.Message, "�"))
	if len(e.What) > 0 {
		m.Key(3)
		m.SetBuf(appendNames(m.Buf(), e.What))
	}
	return m.End()
}

// appendNames writes a map of names in their byte order
func appendNames(dst []byte, named map[string]string) []byte {
	dst = AppendMap(dst, len(named))
	for _, name := range slices.Sorted(maps.Keys(named)) {
		dst = appendName(dst, name, named[name])
	}
	return dst
}

func appendName(dst []byte, name, value string) []byte {
	dst = AppendStr(dst, strings.ToValidUTF8(name, "�"))
	if utf8.ValidString(value) {
		return AppendStr(dst, value)
	}
	return AppendBin(dst, []byte(value))
}

func (e *Error) Decode(body []byte) error {
	d := NewDecoder(body)
	e.decode(&d)
	return d.End()
}

func (e *Error) decode(d *Decoder) {
	for key := range d.Fields() {
		switch key {
		case 1:
			e.Code = Code(d.Str())
		case 2:
			e.Message = d.Str()
		case 3:
			e.What = d.nameMap()
		}
	}
}

// nameMap reads a map of names whose values are str, or bin where the text is
// not UTF-8
func (d *Decoder) nameMap() map[string]string {
	named := map[string]string{}
	for name := range d.Names() {
		if d.Type() == TypeBin {
			named[name] = string(d.Bin())
			continue
		}
		named[name] = d.Str()
	}
	return named
}
