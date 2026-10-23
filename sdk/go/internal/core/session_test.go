package core

// The session without a server: a peer in the test reads the frames the
// client writes and answers with frames of its own, so that an order a
// server makes only by chance is made here.

import (
	"bytes"
	"context"
	"sync"
	"testing"
	"time"

	"tinyshed.org/tinystore/internal/wire"
)

type seen struct {
	header
	body []byte
}

// peer is the other end of a session: what it wrote, and a way to answer.
type peer struct {
	t       *testing.T
	session *Session
	mu      sync.Mutex
	written []byte
}

func newPeer(t *testing.T, welcome wire.Welcome) *peer {
	p := &peer{t: t}
	p.session = NewSession(func(b []byte) error {
		p.mu.Lock()
		p.written = append(p.written, b...)
		p.mu.Unlock()
		return nil
	}, Hello{Client: "test"})
	t.Cleanup(func() { p.session.End(Errorf(Closed, "the test ended")) })
	welcome.Protocol = ProtocolVersion
	p.session.Receive(frame(kindWelcome, 0, 0, 0, wire.Encode(&welcome)))
	return p
}

// until is every frame written since the last look, up to and including the
// first of kind, which the writer is given a few seconds to write.
func (p *peer) until(kind uint8) []seen {
	p.t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for {
		p.mu.Lock()
		var frames []seen
		at := 0
		for len(p.written)-at >= headerSize {
			h := parseHeader(p.written[at:])
			stop := at + headerSize + int(h.length)
			frames = append(frames, seen{h, bytes.Clone(p.written[at+headerSize : stop])})
			at = stop
			if h.kind == kind {
				p.written = p.written[at:]
				p.mu.Unlock()
				return frames
			}
		}
		p.mu.Unlock()
		if time.Now().After(deadline) {
			p.t.Fatalf("no %s written", kindName(kind))
		}
		time.Sleep(time.Millisecond)
	}
}

// pinged is every frame written before the PONG to a PING sent now: what the
// session had to write when the PING came.
func (p *peer) pinged() []seen {
	p.session.Receive(frame(kindPing, 0, 0, 0, make([]byte, 8)))
	frames := p.until(kindPong)
	return frames[:len(frames)-1]
}

func requestsOf(frames []seen) []seen {
	var requests []seen
	for _, f := range frames {
		if f.kind == kindRequest {
			requests = append(requests, f)
		}
	}
	return requests
}

// contract: WIRE-07
func TestARequestAGoAwayRefusedStaysUnsentWhenTheCreditComesBack(t *testing.T) {
	ctx := context.Background()
	p := newPeer(t, wire.Welcome{MaxBody: 1 << 20, InFlight: 4, ConnectionCredit: 1, StreamCredit: 1 << 16})
	p.pinged()
	sent, err := p.session.Open(ctx, 0x0104, []byte{0x80}, true)
	if err != nil {
		t.Fatal(err)
	}
	unsent, err := p.session.Open(ctx, 0x0104, []byte{0x80}, true)
	if err != nil {
		t.Fatal(err)
	}
	if requests := requestsOf(p.pinged()); len(requests) != 1 || requests[0].stream != sent.ID {
		t.Fatalf("%d REQUESTs left before the GOAWAY, not the first alone", len(requests))
	}
	p.session.Receive(frame(kindGoAway, 0, 0, 0, wire.Encode(&wire.GoAway{Code: "unavailable", Message: "closing"})))
	if _, err := unsent.Next(ctx); KindOf(err) != Unavailable {
		t.Fatalf("the REQUEST the GOAWAY refused: %v", err)
	}
	// the server grants credit back as it finishes the streams it still runs
	p.session.Receive(credit(0, 1))
	if requests := requestsOf(p.pinged()); len(requests) != 0 {
		t.Fatalf("the refused REQUEST left on stream %d once the credit came back", requests[0].stream)
	}
	p.session.Receive(frame(kindResponse, flagEnd, 0, sent.ID, wire.Encode(&wire.KvEntry{Found: true})))
	if _, err := sent.Next(ctx); err != nil {
		t.Fatalf("the stream the server ran: %v", err)
	}
	if err := p.session.Ended(); err != nil {
		t.Fatalf("the session ended: %v", err)
	}
}
