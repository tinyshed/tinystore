package wire_test

import (
	"bytes"
	"errors"
	"io"
	"testing"

	"github.com/tinyshed/tinystore/server/wire"
)

// headerOnly serves one frame's header and fails the test at any read after it
type headerOnly struct {
	t      *testing.T
	header []byte
	served bool
}

func (h *headerOnly) Read(p []byte) (int, error) {
	if h.served {
		h.t.Error("the body was read")
		return 0, io.ErrUnexpectedEOF
	}
	h.served = true
	return copy(p, h.header), nil
}

func TestAFrameLargerThanAgreedIsRefusedUnread(t *testing.T) {
	header := wire.AppendHeader(nil, wire.Header{
		Length: 1<<20 + 1, Kind: wire.KindRequest, Flags: wire.FlagEnd,
		Method: 0x0101, Stream: 1,
	})
	r := wire.NewReader(&headerOnly{t: t, header: header}, 1<<20)
	if _, err := r.Header(); !errors.Is(err, wire.ErrProtocol) {
		t.Fatalf("a body past the agreed maximum: %v", err)
	}
}

func TestTheAgreedMaximumCanBeLowered(t *testing.T) {
	frame := wire.AppendFrame(nil, wire.Header{Kind: wire.KindData, Stream: 3}, make([]byte, 100))
	r := wire.NewReader(bytes.NewReader(frame), 1<<20)
	r.SetMaxBody(99)
	if _, err := r.Header(); !errors.Is(err, wire.ErrProtocol) {
		t.Fatalf("a body past the lowered maximum: %v", err)
	}
}

func TestAStreamEndsCleanlyOnlyBetweenFrames(t *testing.T) {
	frame := wire.AppendFrame(nil, wire.Header{Kind: wire.KindData, Stream: 3}, []byte("body"))

	r := wire.NewReader(bytes.NewReader(frame), 1<<20)
	if _, err := r.Header(); err != nil {
		t.Fatal(err)
	}
	if _, err := r.Body(nil); err != nil {
		t.Fatal(err)
	}
	if _, err := r.Header(); !errors.Is(err, io.EOF) {
		t.Fatalf("after the last frame: %v", err)
	}

	for cut := 1; cut < len(frame); cut++ {
		r := wire.NewReader(bytes.NewReader(frame[:cut]), 1<<20)
		_, err := r.Header()
		if err == nil {
			_, err = r.Body(nil)
		}
		if !errors.Is(err, io.ErrUnexpectedEOF) {
			t.Fatalf("a frame cut after %d bytes: %v", cut, err)
		}
	}
}

func TestEveryKindTakesTheHeaderItsRulesAllow(t *testing.T) {
	allowed := []wire.Header{
		{Kind: wire.KindHello},
		{Kind: wire.KindWelcome},
		{Kind: wire.KindRequest, Method: 0x0101, Stream: 1},
		{Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: 0x0101, Stream: 1},
		{Kind: wire.KindResponse, Stream: 1},
		{Kind: wire.KindResponse, Flags: wire.FlagEnd | wire.FlagError, Stream: 1},
		{Kind: wire.KindData, Flags: wire.FlagEnd, Stream: 1},
		{Kind: wire.KindCancel, Stream: 1},
		{Length: 4, Kind: wire.KindCredit},
		{Length: 4, Kind: wire.KindCredit, Stream: 9},
		{Length: 8, Kind: wire.KindPing},
		{Length: 8, Kind: wire.KindPong},
		{Kind: wire.KindGoAway},
	}
	for _, h := range allowed {
		if err := h.Check(); err != nil {
			t.Errorf("%+v: %v", h, err)
		}
	}
}

func TestSetLengthGivesTheBodyAppendedAfterAHeader(t *testing.T) {
	h := wire.Header{Kind: wire.KindResponse, Flags: wire.FlagEnd, Stream: 12}
	frame := wire.AppendHeader(nil, h)
	frame = append(frame, "an answer"...)
	wire.SetLength(frame)
	if want := wire.AppendFrame(nil, h, []byte("an answer")); !bytes.Equal(frame, want) {
		t.Fatalf("%x, want %x", frame, want)
	}
}

func FuzzFrames(f *testing.F) {
	for _, vector := range readVectors(f).Frames {
		f.Add(unhex(f, vector.Hex))
	}
	f.Add(wire.AppendFrame(wire.AppendCredit(nil, 0, 7), wire.Header{Kind: wire.KindData, Stream: 1}, []byte("x")))
	f.Fuzz(func(t *testing.T, data []byte) {
		const maxBody = 64
		r := wire.NewReader(bytes.NewReader(data), maxBody)
		var again []byte
		for {
			h, err := r.Header()
			if err != nil {
				break
			}
			if h.Check() != nil || h.Length > maxBody {
				t.Fatalf("took %+v", h)
			}
			body, err := r.Body(nil)
			if err != nil {
				break
			}
			again = wire.AppendFrame(again, h, body)
		}
		if !bytes.HasPrefix(data, again) {
			t.Fatalf("the frames taken write %x, not a prefix of %x", again, data)
		}
	})
}
