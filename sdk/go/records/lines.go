package records

import (
	"bytes"
	"context"
	"sync"

	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

const (
	// linesHeld is what a writer of lines holds for the store at most: past
	// it, lines are dropped and counted.
	linesHeld = 1 << 20
	// eventRoom is the bytes one event holds: a longer line is dropped whole,
	// as the store drops one.
	eventRoom = 256 << 10
)

// Lines is a writer of another program's output into a stream, as
// Events.Lines begins it: each line becomes an event, and a write never
// waits. It sends whole lines, so that what it has no room for, Dropped
// counts as lines. The store's close hands over what it holds.
type Lines struct {
	store  *core.Store
	stream *core.Stream
	piece  int
	forget func()

	mu         sync.Mutex
	queued     [][]byte
	held       int
	partial    []byte
	discarding bool
	dropped    int
	ended      bool
	failure    error
	wake       chan struct{}
	sent       chan struct{}
}

func newLines(store *core.Store, stream *core.Stream, piece int) *Lines {
	l := &Lines{store: store, stream: stream, piece: piece, wake: make(chan struct{}, 1), sent: make(chan struct{})}
	l.forget = store.OnClose(core.StopLoggers, func() { _ = l.Close() })
	go l.send()
	return l
}

// Dropped is the lines dropped: past what the writer holds, and, once it
// closed, by the store.
func (l *Lines) Dropped() int {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.dropped
}

// Write takes a piece of output, cut anywhere; it never waits.
func (l *Lines) Write(p []byte) (int, error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.ended {
		return 0, core.Errorf(core.Closed, "lines written after their end")
	}
	data := p
	if l.discarding {
		end := bytes.IndexByte(data, '\n')
		if end < 0 {
			return len(p), nil
		}
		l.discarding = false
		data = data[end+1:]
	}
	joined := append(l.partial, data...)
	cut := bytes.LastIndexByte(joined, '\n') + 1
	l.partial = append([]byte(nil), joined[cut:]...)
	if len(l.partial) > eventRoom {
		l.partial = nil
		l.discarding = true
		l.dropped++
	}
	l.queue(joined[:cut])
	return len(p), nil
}

// Close ends the writer: what it holds goes, a last line without its
// newline too, and it returns once the store wrote them.
func (l *Lines) Close() error {
	l.mu.Lock()
	if !l.ended {
		l.ended = true
		l.queue(l.partial)
		l.partial = nil
		l.signal()
	}
	l.mu.Unlock()
	<-l.sent
	l.forget()
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.failure
}

// queue holds bytes for the store, or drops and counts their lines when it
// holds no room; the caller holds mu.
func (l *Lines) queue(data []byte) {
	if len(data) == 0 {
		return
	}
	if l.failure != nil || l.held+len(data) > linesHeld {
		l.dropped += linesIn(data)
		return
	}
	l.queued = append(l.queued, append([]byte(nil), data...))
	l.held += len(data)
	l.signal()
}

func (l *Lines) signal() {
	select {
	case l.wake <- struct{}{}:
	default:
	}
}

// send sends what is held as it comes, in pieces within the stream's
// credit, then the end, and counts what the store dropped.
func (l *Lines) send() {
	defer close(l.sent)
	ctx := context.Background()
	for {
		l.mu.Lock()
		if len(l.queued) == 0 {
			ended := l.ended
			l.mu.Unlock()
			if ended {
				break
			}
			<-l.wake
			continue
		}
		data := l.queued[0]
		l.queued = l.queued[1:]
		l.held -= len(data)
		l.mu.Unlock()
		for at := 0; at < len(data); at += l.piece {
			piece := data[at:min(at+l.piece, len(data))]
			if err := l.stream.Send(ctx, wire.Encode(&wire.RecordsPiece{Bytes: piece}), false); err != nil {
				l.fail(err)
				return
			}
		}
	}
	if err := l.stream.Send(ctx, nil, true); err != nil {
		l.fail(err)
		return
	}
	for {
		event, err := l.stream.Next(ctx)
		if err != nil {
			l.fail(err)
			return
		}
		if event.End {
			var dropped wire.RecordsDropped
			if err := decode(event.Body, &dropped); err != nil {
				l.fail(err)
				return
			}
			l.mu.Lock()
			l.dropped += int(dropped.Dropped)
			l.mu.Unlock()
			return
		}
	}
}

// fail keeps why the writer stopped and counts what it held as dropped.
func (l *Lines) fail(err error) {
	l.stream.Cancel()
	l.mu.Lock()
	defer l.mu.Unlock()
	l.failure = err
	for _, data := range l.queued {
		l.dropped += linesIn(data)
	}
	l.queued, l.held = nil, 0
}

// linesIn is the lines bytes hold: one a newline ends, and one more past the last.
func linesIn(data []byte) int {
	n := bytes.Count(data, []byte{'\n'})
	if len(data) > 0 && data[len(data)-1] != '\n' {
		n++
	}
	return n
}
