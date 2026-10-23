// Package scripted is a server in the test's own process that answers a
// client's frames by a script, so that a test makes an order a real server
// makes only by chance: a connection lost once a commit has left, or while a
// handle opens.
package scripted

import (
	"context"
	"encoding/binary"
	"errors"
	"sync"

	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// The kinds of frame a script reads and answers with, as docs/wire.md numbers them.
const (
	Hello    = 1
	Welcome  = 2
	Request  = 3
	Response = 4
	Data     = 5
)

// The flags of a frame: End on a sender's last frame on its stream, and Error
// beside it on a final frame whose body is a failure.
const (
	End   = 1
	Error = 2
)

const headerSize = 12

// Frame is a frame the client sent, or one the script answers with.
type Frame struct {
	Kind   uint8
	Flags  uint8
	Method uint16
	Stream uint32
	Body   []byte
}

// Script is what the server does with a frame of a stream, a REQUEST or a
// DATA: the frames it answers with, or ended to end the connection there.
type Script func(f Frame) (answers []Frame, ended bool)

// Answer is the RESPONSE that ends a stream with body.
func Answer(stream uint32, body []byte) Frame {
	return Frame{Kind: Response, Flags: End, Stream: stream, Body: body}
}

// Dial dials a connection of its own to a server that answers by script each
// time it is called, so that a link's dial again meets the script again.
func Dial(script Script) func(ctx context.Context) (*core.Connection, error) {
	return func(ctx context.Context) (*core.Connection, error) {
		s := &server{script: script, ready: make(chan struct{})}
		s.session = core.NewSession(s.Write, core.Hello{Client: "test"})
		close(s.ready)
		c := core.NewConnection(s.session, s, nil)
		if err := c.Handshake(ctx); err != nil {
			return nil, err
		}
		return c, nil
	}
}

type server struct {
	script  Script
	session *core.Session
	// ready is closed once session is set: the session writes its HELLO before NewSession returns
	ready chan struct{}
	mu    sync.Mutex
	rest  []byte
}

// Write takes what the client wrote, any part of a frame or several, and
// answers each whole frame as the script says.
func (s *server) Write(b []byte) error {
	<-s.ready
	s.mu.Lock()
	defer s.mu.Unlock()
	s.rest = append(s.rest, b...)
	for len(s.rest) >= headerSize {
		length := int(binary.LittleEndian.Uint32(s.rest))
		if len(s.rest) < headerSize+length {
			return nil
		}
		f := Frame{
			Kind:   s.rest[4],
			Flags:  s.rest[5],
			Method: binary.LittleEndian.Uint16(s.rest[6:]),
			Stream: binary.LittleEndian.Uint32(s.rest[8:]),
			Body:   append([]byte(nil), s.rest[headerSize:headerSize+length]...),
		}
		s.rest = s.rest[headerSize+length:]
		if !s.take(f) {
			return nil
		}
	}
	return nil
}

// take answers a frame, and says whether the connection goes on.
func (s *server) take(f Frame) bool {
	switch f.Kind {
	case Hello:
		welcome := wire.Welcome{
			Protocol: core.ProtocolVersion, Server: "script", Capability: "admin", MaxBody: 1 << 20,
			InFlight: 64, ConnectionCredit: 1 << 20, StreamCredit: 1 << 20,
		}
		s.session.Receive(frameOf(Frame{Kind: Welcome, Body: wire.Encode(&welcome)}))
	case Request, Data:
		answers, ended := s.script(f)
		if ended {
			s.session.End(errors.New("the script ended the connection"))
			return false
		}
		for _, answer := range answers {
			s.session.Receive(frameOf(answer))
		}
	}
	return true
}

func (s *server) Close() error { return nil }

func frameOf(f Frame) []byte {
	out := make([]byte, headerSize+len(f.Body))
	binary.LittleEndian.PutUint32(out, uint32(len(f.Body)))
	out[4], out[5] = f.Kind, f.Flags
	binary.LittleEndian.PutUint16(out[6:], f.Method)
	binary.LittleEndian.PutUint32(out[8:], f.Stream)
	copy(out[headerSize:], f.Body)
	return out
}
