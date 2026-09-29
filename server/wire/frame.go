package wire

import (
	"bufio"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
)

// HeaderSize is a frame's header, little-endian, before its body:
//
//	0             4      5       6          8             12
//	│ body length │ kind │ flags │ method   │ stream      │ body …
//	  u32           u8     u8      u16        u32
const HeaderSize = 12

// Kind is what a frame is.
type Kind uint8

const (
	KindHello    Kind = 1
	KindWelcome  Kind = 2
	KindRequest  Kind = 3
	KindResponse Kind = 4
	KindData     Kind = 5
	KindCancel   Kind = 6
	KindCredit   Kind = 7
	KindPing     Kind = 8
	KindPong     Kind = 9
	KindGoAway   Kind = 10
)

func (k Kind) String() string {
	if rule, known := rules[k]; known {
		return rule.name
	}
	return fmt.Sprintf("kind %d", uint8(k))
}

// Flags say where a frame stands in its stream.
type Flags uint8

const (
	// FlagEnd marks the sender's last frame on its stream.
	FlagEnd Flags = 1
	// FlagError, only beside FlagEnd, says the body is an Error.
	FlagError Flags = 2
)

// Method is an operation: its high byte names the engine and its low byte the
// operation. A new engine takes a new high byte, so the frame stays as it is.
type Method uint16

type Header struct {
	Length uint32 // the body's
	Kind   Kind
	Flags  Flags
	Method Method
	Stream uint32
}

// ErrProtocol is a frame or a handshake that breaks the protocol; the
// connection ends with it.
var ErrProtocol = errors.New("protocol error")

func protocolf(format string, args ...any) error {
	return fmt.Errorf("%w: "+format, append([]any{ErrProtocol}, args...)...)
}

// rule is what a kind's header may hold: whether it names a stream, the flags
// it defines and the length its body must have, anyLength when any will do
type rule struct {
	name   string
	stream streamRule
	flags  Flags
	length int
}

type streamRule uint8

const (
	noStream     streamRule = iota // stream 0
	ownStream                      // a stream from 1
	eitherStream                   // a stream, or 0 for the connection
)

const anyLength = -1

var rules = map[Kind]rule{
	KindHello:    {"HELLO", noStream, 0, anyLength},
	KindWelcome:  {"WELCOME", noStream, 0, anyLength},
	KindRequest:  {"REQUEST", ownStream, FlagEnd, anyLength},
	KindResponse: {"RESPONSE", ownStream, FlagEnd | FlagError, anyLength},
	KindData:     {"DATA", ownStream, FlagEnd | FlagError, anyLength},
	KindCancel:   {"CANCEL", ownStream, 0, 0},
	KindCredit:   {"CREDIT", eitherStream, 0, 4},
	KindPing:     {"PING", noStream, 0, 8},
	KindPong:     {"PONG", noStream, 0, 8},
	KindGoAway:   {"GOAWAY", noStream, 0, anyLength},
}

// Check refuses a header that breaks the protocol:
//
//	an unknown kind
//	a flag its kind does not define
//	ERROR without END
//	a method outside a REQUEST, or none in one
//	a stream where the kind names none, or none where it names one
//	a body of a length the kind cannot have
func (h Header) Check() error {
	rule, known := rules[h.Kind]
	switch {
	case !known:
		return protocolf("a frame of kind %d", h.Kind)
	case h.Flags&^rule.flags != 0:
		return protocolf("flags %#x on a %s", uint8(h.Flags), h.Kind)
	case h.Flags&FlagError != 0 && h.Flags&FlagEnd == 0:
		return protocolf("ERROR without END on stream %d", h.Stream)
	case h.Kind == KindRequest && h.Method == 0:
		return protocolf("a REQUEST without a method on stream %d", h.Stream)
	case h.Kind != KindRequest && h.Method != 0:
		return protocolf("method %#x on a %s", uint16(h.Method), h.Kind)
	case rule.stream == noStream && h.Stream != 0, rule.stream == ownStream && h.Stream == 0:
		return protocolf("stream %d on a %s", h.Stream, h.Kind)
	case rule.length != anyLength && int64(h.Length) != int64(rule.length):
		return protocolf("a %s of %d bytes, not %d", h.Kind, h.Length, rule.length)
	}
	return nil
}

// AppendHeader appends h, whose Length SetLength may fix once the body is
// appended after it.
func AppendHeader(dst []byte, h Header) []byte {
	dst = binary.LittleEndian.AppendUint32(dst, h.Length)
	dst = append(dst, byte(h.Kind), byte(h.Flags))
	dst = binary.LittleEndian.AppendUint16(dst, uint16(h.Method))
	return binary.LittleEndian.AppendUint32(dst, h.Stream)
}

// SetLength writes the length of the body that follows a frame's header.
func SetLength(frame []byte) {
	binary.LittleEndian.PutUint32(frame, uint32(len(frame)-HeaderSize)) //nolint:gosec // a body within its bound
}

func AppendFrame(dst []byte, h Header, body []byte) []byte {
	h.Length = uint32(len(body)) //nolint:gosec // a body within its bound
	return append(AppendHeader(dst, h), body...)
}

// ParseHeader reads a header from its twelve bytes without checking it.
func ParseHeader(b []byte) Header {
	return Header{
		Length: binary.LittleEndian.Uint32(b),
		Kind:   Kind(b[4]),
		Flags:  Flags(b[5]),
		Method: Method(binary.LittleEndian.Uint16(b[6:])),
		Stream: binary.LittleEndian.Uint32(b[8:]),
	}
}

// AppendCredit appends a CREDIT granting n bytes on stream, 0 for the connection.
func AppendCredit(dst []byte, stream, n uint32) []byte {
	dst = AppendHeader(dst, Header{Length: 4, Kind: KindCredit, Stream: stream})
	return binary.LittleEndian.AppendUint32(dst, n)
}

func Granted(body []byte) uint32 {
	return binary.LittleEndian.Uint32(body)
}

// Reader reads frames from a stream of bytes. A header that breaks the
// protocol, or announces a body past the agreed maximum, is refused before a
// byte of its body is read.
type Reader struct {
	r       *bufio.Reader
	maxBody uint32
	header  [HeaderSize]byte
	pending uint32 // the body the last header announced, not yet read
}

// the reader's buffer: frames of a few hundred bytes arrive many to a read
const readBuffer = 64 << 10

func NewReader(r io.Reader, maxBody uint32) *Reader {
	return &Reader{r: bufio.NewReaderSize(r, readBuffer), maxBody: maxBody}
}

// SetMaxBody changes the largest body a frame may carry, as a handshake agrees it.
func (r *Reader) SetMaxBody(n uint32) {
	r.maxBody = n
}

// Header reads and checks the next frame's header; Body reads the body it
// announces. The end of the stream between two frames is io.EOF.
func (r *Reader) Header() (Header, error) {
	if r.pending != 0 {
		return Header{}, errors.New("wire: a header read before the last frame's body")
	}
	if _, err := io.ReadFull(r.r, r.header[:]); err != nil {
		return Header{}, err
	}
	h := ParseHeader(r.header[:])
	if err := h.Check(); err != nil {
		return Header{}, err
	}
	if h.Length > r.maxBody {
		return Header{}, protocolf("a %s of %d bytes, %d agreed", h.Kind, h.Length, r.maxBody)
	}
	r.pending = h.Length
	return h, nil
}

// Body reads the body the last header announced into room, grown when it is
// too small, so that the caller decides where bodies live.
func (r *Reader) Body(room []byte) ([]byte, error) {
	n := int(r.pending)
	if cap(room) < n {
		room = make([]byte, n)
	}
	body := room[:n]
	r.pending = 0
	if _, err := io.ReadFull(r.r, body); err != nil {
		return nil, unexpected(err)
	}
	return body, nil
}

// unexpected is the end of the stream inside a frame, which is never clean
func unexpected(err error) error {
	if errors.Is(err, io.EOF) {
		return io.ErrUnexpectedEOF
	}
	return err
}
