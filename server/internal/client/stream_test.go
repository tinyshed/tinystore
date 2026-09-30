package client

import (
	"context"
	"errors"
	"io"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/server/internal/flow"
	"github.com/tinyshed/tinystore/server/wire"
)

func TestAFinalResponseStopsAnUploadWaitingForCredit(t *testing.T) {
	for _, waiting := range []string{"stream", "connection"} {
		t.Run(waiting, func(t *testing.T) { refusedUpload(t, waiting) })
	}
}

func refusedUpload(t *testing.T, waiting string) {
	t.Helper()
	c, st := waitingUpload(waiting)
	ctx, cancel := context.WithTimeout(t.Context(), time.Second)
	defer cancel()

	sent := make(chan error, 1)
	go func() { sent <- st.Send(ctx, []byte("body"), false) }()
	failure := wire.Error{Code: wire.CodeLimit, Message: "the upload was refused"}
	c.deliver(wire.Header{Kind: wire.KindResponse, Flags: wire.FlagEnd | wire.FlagError, Stream: st.id},
		failure.Append(nil))

	err := <-sent
	ended, ok := errors.AsType[*wire.Error](err)
	if !ok || ended.Code != failure.Code || ended.Message != failure.Message {
		t.Fatalf("the upload waiting for credit: %v", err)
	}
	if _, err = st.Response(ctx); !errors.As(err, &ended) || ended.Code != failure.Code {
		t.Fatalf("the response that ended the upload: %v", err)
	}
}

func TestALostConnectionStopsAnUploadWaitingForCredit(t *testing.T) {
	c, st := waitingUpload("stream")
	ctx, cancel := context.WithTimeout(t.Context(), time.Second)
	defer cancel()

	sent := make(chan error, 1)
	go func() { sent <- st.Send(ctx, []byte("body"), false) }()
	c.receive()

	if err := <-sent; err == nil || errors.Is(err, context.DeadlineExceeded) ||
		!strings.Contains(err.Error(), "the connection ended") {
		t.Fatalf("the upload waiting for credit on a lost connection: %v", err)
	}
}

func waitingUpload(waiting string) (*Conn, *Stream) {
	c := &Conn{
		reader: wire.NewReader(strings.NewReader(""), 1024), writer: flow.NewWriter(io.Discard, 1024),
		credit: flow.NewAllowance(1024), streams: map[uint32]*Stream{}, read: make(chan struct{}),
	}
	st := &Stream{
		conn: c, id: 1, sends: flow.NewAllowance(0), ready: make(chan struct{}, 1), uploadDone: make(chan struct{}),
	}
	if waiting == "connection" {
		st.sends = flow.NewAllowance(1024)
		c.credit = flow.NewAllowance(0)
	}
	c.streams[st.id] = st
	return c, st
}
