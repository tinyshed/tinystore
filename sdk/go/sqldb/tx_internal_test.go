package sqldb

// A transaction whose connection is lost, against a server that answers by a
// script: a loss before the commit left and one after it, which only a
// script can place.

import (
	"context"
	"errors"
	"strings"
	"testing"

	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/scripted"
	"tinyshed.org/tinystore/internal/wire"
)

// committing opens the database, begins the transaction, answers each
// statement, and ends the connection at the commit.
func committing(f scripted.Frame) ([]scripted.Frame, bool) {
	switch {
	case f.Kind == scripted.Request && f.Method == wire.MethodSqlOpen:
		return []scripted.Frame{scripted.Answer(f.Stream, wire.Encode(&wire.Handle{Handle: 1}))}, false
	case f.Kind == scripted.Request && f.Method == wire.MethodSqlTx:
		return []scripted.Frame{{Kind: scripted.Response, Stream: f.Stream, Body: wire.Encode(&wire.Empty{})}}, false
	}
	var call wire.SqlTxCall
	if err := wire.Decode(f.Body, &call); err != nil || call.Commit {
		return nil, true
	}
	done := wire.Encode(&wire.SqlTxAnswer{Done: &wire.SqlDone{Changes: 1}})
	return []scripted.Frame{{Kind: scripted.Data, Stream: f.Stream, Body: done}}, false
}

func scriptedDB(dial func(ctx context.Context) (*core.Connection, error)) *DB {
	return &DB{
		store: core.NewStore(core.NewLink(dial)), name: "app", describe: "sql app",
		opener: &core.Opener{Method: wire.MethodSqlOpen, Body: wire.Encode(&wire.SqlOpen{Name: "app"})},
		guests: map[string]*core.Opener{},
	}
}

// contract: WIRE-05
func TestATransactionWhoseCommitLeftUnansweredHasAnUnknownOutcome(t *testing.T) {
	db := scriptedDB(scripted.Dial(committing))
	defer db.store.Close()
	err := db.Tx(context.Background(), func(tx *Tx) error {
		_, err := tx.Exec(context.Background(), "update items set stock = stock - 1 where sku = 'a'")
		return err
	})
	if core.KindOf(err) != core.OutcomeUnknown || !strings.Contains(err.Error(), "sql app") {
		t.Fatalf("a commit lost with no answer: %v", err)
	}
}

// contract: WIRE-05
func TestATransactionWhoseCommitsWaitIsCancelledHasAnUnknownOutcome(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	db := scriptedDB(scripted.Dial(func(f scripted.Frame) ([]scripted.Frame, bool) {
		var call wire.SqlTxCall
		if f.Kind == scripted.Data && wire.Decode(f.Body, &call) == nil && call.Commit {
			// the commit has left, and the program gives up waiting for its answer
			cancel()
			return nil, false
		}
		return committing(f)
	}))
	defer db.store.Close()
	err := db.Tx(ctx, func(tx *Tx) error { return nil })
	if core.KindOf(err) != core.OutcomeUnknown || !errors.Is(err, context.Canceled) {
		t.Fatalf("a commit whose wait was cancelled: %v", err)
	}
}

// contract: WIRE-06
func TestATransactionLostBeforeItsCommitLeftIsClosedSinceTheServerRollsItBack(t *testing.T) {
	var dialled *core.Connection
	dial := scripted.Dial(committing)
	db := scriptedDB(func(ctx context.Context) (*core.Connection, error) {
		c, err := dial(ctx)
		dialled = c
		return c, err
	})
	defer db.store.Close()
	err := db.Tx(context.Background(), func(tx *Tx) error {
		if _, err := tx.Exec(context.Background(), "update items set stock = stock - 1 where sku = 'a'"); err != nil {
			return err
		}
		dialled.Session.End(errors.New("reset"))
		return nil
	})
	if core.KindOf(err) != core.Closed {
		t.Fatalf("a transaction lost before its commit left: %v", err)
	}
}
