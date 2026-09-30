package client

import (
	"context"
	"errors"
	"fmt"
	"sync"

	"github.com/tinyshed/tinystore/server/internal/flow"
	"github.com/tinyshed/tinystore/server/wire"
)

// Stream is one stream as the client sees it: what it uploads, and the
// RESPONSE and DATA the server sends on it, waiting until they are taken.
type Stream struct {
	conn   *Conn
	id     uint32
	sends  *flow.Allowance // what the client may still upload; nil when its REQUEST ended its side
	window *flow.Credit    // what the client took of the server's DATA, to grant back

	mu         sync.Mutex
	frames     []frame
	ready      chan struct{}
	ended      bool // the final frame was taken
	uploadDone chan struct{}
	uploadErr  error
}

type frame struct {
	header wire.Header
	body   []byte
	err    error
}

func (st *Stream) ID() uint32 {
	return st.id
}

func (st *Stream) push(f frame) {
	st.mu.Lock()
	st.frames = append(st.frames, f)
	st.mu.Unlock()
	select {
	case st.ready <- struct{}{}:
	default:
	}
}

func (st *Stream) next(ctx context.Context) (frame, error) {
	for {
		st.mu.Lock()
		if st.ended {
			st.mu.Unlock()
			return frame{}, errors.New("client: the stream has ended")
		}
		if len(st.frames) > 0 {
			f := st.frames[0]
			st.frames = st.frames[1:]
			st.ended = f.err != nil || f.header.Flags&wire.FlagEnd != 0
			st.mu.Unlock()
			return f, f.err
		}
		st.mu.Unlock()
		select {
		case <-st.ready:
		case <-ctx.Done():
			return frame{}, ctx.Err()
		}
	}
}

// Response waits for the stream's RESPONSE: its body, or the *wire.Error it
// ends the stream with.
func (st *Stream) Response(ctx context.Context) ([]byte, error) {
	f, err := st.next(ctx)
	if err != nil {
		return nil, err
	}
	if f.header.Kind != wire.KindResponse {
		return nil, fmt.Errorf("client: a %s where the RESPONSE belongs", f.header.Kind)
	}
	return failed(f)
}

// Next waits for the next DATA: its body, and whether it ended the stream. It
// grants back what it takes, as a consumer that has room again.
func (st *Stream) Next(ctx context.Context) (body []byte, last bool, err error) {
	f, err := st.next(ctx)
	if err != nil {
		return nil, false, err
	}
	if f.header.Kind != wire.KindData {
		return nil, false, fmt.Errorf("client: a %s where DATA belongs", f.header.Kind)
	}
	last = f.header.Flags&wire.FlagEnd != 0
	if body, err = failed(f); err != nil {
		return nil, true, err
	}
	if grant := st.window.Consume(int64(len(body))); grant > 0 && !last {
		err = st.conn.writer.Send(wire.AppendCredit(nil, st.id, uint32(grant))) //nolint:gosec // within the window
	}
	return body, last, err
}

// failed is a final frame's error, or its body
func failed(f frame) ([]byte, error) {
	if f.header.Flags&wire.FlagError == 0 {
		return f.body, nil
	}
	var failure wire.Error
	if err := failure.Decode(f.body); err != nil {
		return nil, err
	}
	return nil, &failure
}

// Send uploads one DATA body, within the stream's credit and the
// connection's; end says it is the last.
func (st *Stream) Send(ctx context.Context, body []byte, end bool) error {
	if st.sends == nil {
		return errors.New("client: a stream whose REQUEST ended the client's side")
	}
	if err := st.sends.TakeUntil(ctx, int64(len(body)), st.uploadDone); err != nil {
		return st.sendError(err)
	}
	if err := st.conn.credit.TakeUntil(ctx, int64(len(body)), st.uploadDone); err != nil {
		st.sends.Grant(int64(len(body)))
		return st.sendError(err)
	}
	flags := wire.Flags(0)
	if end {
		flags = wire.FlagEnd
	}
	return st.conn.writer.Send(wire.AppendFrame(nil, wire.Header{Kind: wire.KindData, Flags: flags, Stream: st.id},
		body))
}

func (st *Stream) endUpload(err error) {
	if st.sends == nil {
		return
	}
	st.mu.Lock()
	defer st.mu.Unlock()
	if st.uploadErr == nil {
		if err == nil {
			err = errors.New("client: the stream ended")
		}
		st.uploadErr = err
		close(st.uploadDone)
	}
}

func (st *Stream) sendError(err error) error {
	if !errors.Is(err, flow.ErrEnded) {
		return err
	}
	st.mu.Lock()
	defer st.mu.Unlock()
	return st.uploadErr
}

// Cancel asks the server to stop the stream, which still ends with its final
// frame.
func (st *Stream) Cancel() error {
	return st.conn.writer.Send(wire.AppendFrame(nil, wire.Header{Kind: wire.KindCancel, Stream: st.id}, nil))
}
