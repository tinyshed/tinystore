package server

import (
	"testing"
	"time"

	"github.com/tinyshed/tinystore/server/wire"
)

// A server stops when an admin asks and its program said how: it answers,
// then its program's Stop runs. A data connection's stop is refused, and so
// is a stop of a server whose program said nothing of stopping.
func TestAServerStopsAtAnAdminsRequestOnly(t *testing.T) {
	data := newToken(t)
	tokens, err := ParseTokens([]byte("data " + data))
	if err != nil {
		t.Fatal(err)
	}
	stopped := make(chan struct{}, 2)
	ts := startTestServer(t, Options{Tokens: tokens, Stop: func() { stopped <- struct{}{} }})

	remote := ts.dialAt(t, ts.remote, wire.Hello{Token: data})
	if _, err = remote.Call(t.Context(), wire.ServerStop, wire.Empty{}); failureOf(err).Code != wire.CodePermission {
		t.Errorf("a data connection's stop: %v", err)
	}
	select {
	case <-stopped:
		t.Fatal("a data connection stopped the server")
	default:
	}

	admin := ts.dial(t, wire.Hello{})
	if _, err = admin.Call(t.Context(), wire.ServerStop, wire.Empty{}); err != nil {
		t.Fatal(err)
	}
	select {
	case <-stopped:
	case <-time.After(10 * time.Second):
		t.Fatal("the program's Stop never ran")
	}

	unstoppable := startTestServer(t, Options{})
	_, err = unstoppable.dial(t, wire.Hello{}).Call(t.Context(), wire.ServerStop, wire.Empty{})
	if failureOf(err).Code != wire.CodePermission {
		t.Errorf("a stop of a server whose program said nothing of stopping: %v", err)
	}
}
