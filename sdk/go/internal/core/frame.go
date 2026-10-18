package core

import (
	"encoding/binary"
	"fmt"
)

// A frame is a twelve-byte header, little-endian, and a body:
//
//	0             4      5       6          8             12
//	│ body length │ kind │ flags │ method   │ stream      │ body …
//	  u32           u8     u8      u16        u32
const headerSize = 12

// The kinds of frame, as docs/wire.md numbers them.
const (
	kindHello = iota + 1
	kindWelcome
	kindRequest
	kindResponse
	kindData
	kindCancel
	kindCredit
	kindPing
	kindPong
	kindGoAway
)

// The flags of a frame.
const (
	// flagEnd is the sender's last frame on its stream.
	flagEnd = 1
	// flagError beside flagEnd alone: the body is an error.
	flagError = 2
)

type header struct {
	length uint32
	kind   uint8
	flags  uint8
	method uint16
	stream uint32
}

// rule is what a kind of frame may carry: a stream of none, its own or
// either, the flags it defines, and the length its body must have, -1 for any.
type rule struct {
	name   string
	stream string
	flags  uint8
	length int
}

var rules = map[uint8]rule{
	kindHello:    {"HELLO", "none", 0, -1},
	kindWelcome:  {"WELCOME", "none", 0, -1},
	kindRequest:  {"REQUEST", "own", flagEnd, -1},
	kindResponse: {"RESPONSE", "own", flagEnd | flagError, -1},
	kindData:     {"DATA", "own", flagEnd | flagError, -1},
	kindCancel:   {"CANCEL", "own", 0, 0},
	kindCredit:   {"CREDIT", "either", 0, 4},
	kindPing:     {"PING", "none", 0, 8},
	kindPong:     {"PONG", "none", 0, 8},
	kindGoAway:   {"GOAWAY", "none", 0, -1},
}

func kindName(kind uint8) string {
	if r, ok := rules[kind]; ok {
		return r.name
	}
	return fmt.Sprintf("kind %d", kind)
}

func parseHeader(b []byte) header {
	return header{
		length: binary.LittleEndian.Uint32(b),
		kind:   b[4],
		flags:  b[5],
		method: binary.LittleEndian.Uint16(b[6:]),
		stream: binary.LittleEndian.Uint32(b[8:]),
	}
}

// check refuses a header that breaks the protocol: an unknown kind, a flag
// its kind does not define, ERROR without END, a method outside a REQUEST or
// none in one, a stream where the kind names none or none where it names one,
// and a body of a length the kind cannot have or past the agreed maximum.
func (h header) check(maxBody uint64) error {
	r, ok := rules[h.kind]
	switch {
	case !ok:
		return Errorf(Protocol, "a frame of kind %d", h.kind)
	case h.flags&^r.flags != 0:
		return Errorf(Protocol, "flags %#x on a %s", h.flags, r.name)
	case h.flags&flagError != 0 && h.flags&flagEnd == 0:
		return Errorf(Protocol, "ERROR without END on stream %d", h.stream)
	case h.kind == kindRequest && h.method == 0:
		return Errorf(Protocol, "a REQUEST without a method on stream %d", h.stream)
	case h.kind != kindRequest && h.method != 0:
		return Errorf(Protocol, "method %#x on a %s", h.method, r.name)
	case r.stream == "none" && h.stream != 0, r.stream == "own" && h.stream == 0:
		return Errorf(Protocol, "stream %d on a %s", h.stream, r.name)
	case r.length >= 0 && int(h.length) != r.length:
		return Errorf(Protocol, "a %s of %d bytes, not %d", r.name, h.length, r.length)
	case uint64(h.length) > maxBody:
		return Errorf(Protocol, "a %s of %d bytes, %d agreed", r.name, h.length, maxBody)
	}
	return nil
}

func frame(kind, flags uint8, method uint16, stream uint32, body []byte) []byte {
	out := make([]byte, headerSize+len(body))
	binary.LittleEndian.PutUint32(out, uint32(len(body)))
	out[4], out[5] = kind, flags
	binary.LittleEndian.PutUint16(out[6:], method)
	binary.LittleEndian.PutUint32(out[8:], stream)
	copy(out[headerSize:], body)
	return out
}

// credit is a CREDIT granting n bytes on a stream, or on the connection for 0.
func credit(stream uint32, n uint64) []byte {
	var body [4]byte
	binary.LittleEndian.PutUint32(body[:], uint32(min(n, 1<<32-1)))
	return frame(kindCredit, 0, 0, stream, body[:])
}
