package sqldb

import (
	"context"
	"iter"
	"sync"

	"github.com/tinyshed/tinystore/internal/core"
	"github.com/tinyshed/tinystore/internal/wire"
)

// Tx is a database's transaction, from db.Tx. Its reads see its writes, a
// call that fails leaves it as it was before the call, and its calls go one
// at a time, in order. A bucket, a queue or a channel takes it in with In:
// bucket.In(tx), and what they write commits with the rows.
type Tx struct {
	db     *DB
	conn   *core.Connection
	stream *core.Stream
	turn   sync.Mutex
	// opened are the handles kept in the file that opened on the connection
	// before the transaction held the writer, and what refused them
	opened map[*core.Opener]error
	ended  bool
}

// Tx runs fn in a transaction that holds the writer, for writes that depend
// on what its reads found: an error fn returns rolls it back, and is Tx's. It
// runs once, five seconds at most, past which the store rolls it back;
// inside it, call nothing that waits on the world.
func (db *DB) Tx(ctx context.Context, fn func(tx *Tx) error) error {
	c, err := db.store.Link.Connection(ctx)
	if err != nil {
		return err
	}
	handle, err := db.Handle(ctx, c)
	if err != nil {
		return err
	}
	opened := db.openGuests(ctx, c)
	stream, err := c.Session.Open(ctx, wire.MethodSqlTx, wire.Encode(&wire.SqlTxOpen{Handle: handle}), false)
	if err != nil {
		return err
	}
	if _, err := stream.Next(ctx); err != nil {
		return err
	}
	tx := &Tx{db: db, conn: c, stream: stream, opened: opened}
	if err := fn(tx); err != nil {
		_ = tx.end(ctx, false)
		return err
	}
	return tx.end(ctx, true)
}

// openGuests opens on a connection the buckets, queues and indexes kept in
// the file, before a transaction holds its writer: a first open writes the
// file, and would wait for the writer the transaction holds. One that fails
// is told when the transaction takes it in.
func (db *DB) openGuests(ctx context.Context, c *core.Connection) map[*core.Opener]error {
	db.mu.Lock()
	guests := make([]*core.Opener, 0, len(db.guests))
	for _, opener := range db.guests {
		guests = append(guests, opener)
	}
	db.mu.Unlock()
	opened := make(map[*core.Opener]error, len(guests))
	var mu sync.Mutex
	var opening sync.WaitGroup
	for _, opener := range guests {
		opening.Add(1)
		go func() {
			defer opening.Done()
			_, err := core.HandleOn(ctx, c, opener)
			mu.Lock()
			opened[opener] = err
			mu.Unlock()
		}()
	}
	opening.Wait()
	return opened
}

// CoreTx is how this module's packages take a handle into the transaction; a
// program has no use for it.
func (tx *Tx) CoreTx() core.TxOf { return core.TxOf{Database: tx} }

// Database is the database the transaction is of.
func (tx *Tx) Database() core.Database { return tx.db }

// Via is how a handle's calls go inside the transaction, on its stream.
func (tx *Tx) Via() core.Via { return txVia{tx} }

// Nested is a call of kv, jobs or channels in the transaction: its method,
// its request, and the method's answer.
func (tx *Tx) Nested(ctx context.Context, method uint16, body []byte) ([]byte, error) {
	m := uint64(method)
	answer, err := tx.send(ctx, wire.SqlTxCall{Want: "call", Method: &m, Body: &body})
	if err != nil || answer.Body == nil {
		return nil, err
	}
	return *answer.Body, nil
}

type txVia struct{ tx *Tx }

func (v txVia) Call(ctx context.Context, opener *core.Opener, method uint16, body func(handle uint64) []byte, _ core.Idempotence) ([]byte, error) {
	refused, opened := v.tx.opened[opener]
	switch {
	case opener.Make == nil:
		return nil, core.Errorf(core.Invalid, "%s: a handle kept outside the database, in its transaction", v.tx.db.describe)
	case !opened:
		return nil, core.Errorf(core.Invalid,
			"%s: a handle opened after the transaction began, whose first open would wait for the writer it holds: open it before db.Tx", v.tx.db.describe)
	case refused != nil:
		return nil, refused
	}
	handle, err := core.HandleOn(ctx, v.tx.conn, opener)
	if err != nil {
		return nil, err
	}
	return v.tx.Nested(ctx, method, body(handle))
}

func (tx *Tx) describeIt() string { return tx.db.describe }

func (tx *Tx) rows(ctx context.Context, statement Piece, want string) (answered, error) {
	if statement.err != nil {
		return answered{}, statement.err
	}
	answer, err := tx.send(ctx, wire.SqlTxCall{Text: statement.text, Values: statement.values, Want: want})
	if err != nil || answer.Rows == nil {
		return answered{}, err
	}
	return answered{answer.Rows.Columns, answer.Rows.Rows}, nil
}

func (tx *Tx) exec(ctx context.Context, statement Piece) (Result, error) {
	if statement.err != nil {
		return Result{}, statement.err
	}
	answer, err := tx.send(ctx, wire.SqlTxCall{Text: statement.text, Values: statement.values, Want: "exec"})
	if err != nil || answer.Done == nil {
		return Result{}, err
	}
	return resultOf(*answer.Done), nil
}

func (tx *Tx) each(ctx context.Context, statement Piece) iter.Seq2[[]any, error] {
	return func(yield func([]any, error) bool) {
		got, err := tx.rows(ctx, statement, "all")
		if err != nil {
			yield(nil, err)
			return
		}
		if !yield(append([]any{columnsMark{}}, toAny(got.columns)...), nil) {
			return
		}
		for _, row := range got.rows {
			if !yield(row, nil) {
				return
			}
		}
	}
}

// Exec runs a write in the transaction.
func (tx *Tx) Exec(ctx context.Context, text string, args ...any) (Result, error) {
	return tx.exec(ctx, statementOf(text, args))
}

// send sends one call of the transaction and reads its answer, one call at a time.
func (tx *Tx) send(ctx context.Context, call wire.SqlTxCall) (wire.SqlTxAnswer, error) {
	tx.turn.Lock()
	defer tx.turn.Unlock()
	if tx.ended {
		return wire.SqlTxAnswer{}, core.Errorf(core.Invalid, "%s: a call of a transaction that ended", tx.db.describe)
	}
	if err := tx.stream.Send(ctx, wire.Encode(&call), false); err != nil {
		return wire.SqlTxAnswer{}, err
	}
	event, err := tx.stream.Next(ctx)
	if err != nil {
		return wire.SqlTxAnswer{}, err
	}
	tx.stream.Consumed(len(event.Body))
	var answer wire.SqlTxAnswer
	if err := decode(event.Body, &answer); err != nil {
		return answer, err
	}
	if answer.Failure != nil {
		var what map[string]string
		if answer.Failure.What != nil {
			what = *answer.Failure.What
		}
		return answer, core.Failed(answer.Failure.Code, answer.Failure.Message, what)
	}
	return answer, nil
}

// end sends the last call, commit or roll back, and waits for the store's word.
func (tx *Tx) end(ctx context.Context, commit bool) error {
	tx.turn.Lock()
	defer tx.turn.Unlock()
	if tx.ended {
		return nil
	}
	tx.ended = true
	if err := tx.stream.Send(ctx, wire.Encode(&wire.SqlTxCall{Commit: commit}), true); err != nil {
		return err
	}
	_, err := tx.stream.Next(ctx)
	return err
}
