package core

import (
	"context"
	"errors"
	"strings"
	"sync"

	"tinyshed.org/tinystore/internal/wire"
)

// A transaction of the store's own files never holds the writer while the
// program runs: a read goes at once and is kept with what it found; a write
// waits, and when the function returns the writes commit in one call, after a
// check that every key it read is still as it read it. A check that fails
// runs the function again, TxRuns times at most.

// TxRuns is how many times a transaction runs its function before a key it
// read that keeps changing is a conflict.
const TxRuns = 5

// KvPart is what a transaction needs of a kv handle it takes in.
type KvPart struct {
	Link   *Link
	Name   string
	Opener *Opener
	Under  []string
	// Home is the database whose file keeps the handle; nil for kv.db
	Home Database
}

// place is a key's place: its handle, its owners and itself, each kept apart.
func (p KvPart) place(key string) string {
	return string(rune(p.Opener.Method)) + "\x00" + string(p.Opener.Body) + "\x00" +
		strings.Join(p.Under, "\x01") + "\x00" + key
}

func (p KvPart) handlePlace() string {
	return string(rune(p.Opener.Method)) + "\x00" + string(p.Opener.Body)
}

// JobOp is a write of a queue the store's transaction keeps for its commit:
// its method, and its call or the id it cancels.
type JobOp struct {
	Opener *Opener
	Method uint16
	Call   *wire.JobsCall
	ID     string
}

// StoreTx is the store's transaction while its function runs.
type StoreTx struct {
	store   *Store
	mu      sync.Mutex
	reads   map[string]*txRead
	order   []string
	written map[string]txWritten
	cleared []txCleared
	ops     []txOp
	jobs    []JobOp
	after   []func(ctx context.Context) error
	file    string
	done    bool
}

type txRead struct {
	part  KvPart
	key   string
	done  chan struct{}
	entry wire.KvEntry
	err   error
}

type txWritten struct {
	found wire.KvEntry
	at    int
}

type txCleared struct {
	handle string
	under  []string
	at     int
}

type txOp struct {
	method uint16
	part   KvPart
	call   wire.KvCall
}

// RunTx runs fn as one transaction and gives back its error: its writes all
// commit or none do.
func RunTx(ctx context.Context, store *Store, fn func(tx *StoreTx) error) error {
	for run := 1; ; run++ {
		tx := &StoreTx{store: store, reads: map[string]*txRead{}, written: map[string]txWritten{}}
		if err := fn(tx); err != nil {
			return err
		}
		err := tx.commit(ctx)
		var e *Error
		if err == nil || run >= TxRuns || !errors.As(err, &e) || e.Kind != Conflict {
			return err
		}
		if _, read := e.What["check"]; !read {
			return err
		}
	}
}

func (tx *StoreTx) open() error {
	if tx.done {
		return Errorf(Invalid, "a transaction takes its calls while its function runs, before it commits")
	}
	return nil
}

// Takes refuses a handle of another store, one kept in a database's file, and
// one of the store's other file.
func (tx *StoreTx) Takes(link *Link, home Database, file, what string) error {
	tx.mu.Lock()
	defer tx.mu.Unlock()
	if link != tx.store.Link {
		return Errorf(Invalid, "%s: a handle of another store, in a transaction of this one", what)
	}
	if home != nil {
		return Errorf(Invalid, "%s: %s: kept in the database's file, outside this transaction of the store: take it in with the database's Tx",
			home.Describe(), what)
	}
	if tx.file != "" && tx.file != file {
		return Errorf(Invalid, "%s: kept in %s, outside this transaction of %s: open both from a database to commit them together",
			what, file, tx.file)
	}
	tx.file = file
	return nil
}

var absent = wire.KvEntry{}

// Read is what the key holds as this transaction sees it: its own writes over
// what the store held.
func (tx *StoreTx) Read(ctx context.Context, part KvPart, key string) (wire.KvEntry, error) {
	tx.mu.Lock()
	if err := tx.open(); err != nil {
		tx.mu.Unlock()
		return absent, err
	}
	if own, found := tx.own(part, key); found {
		tx.mu.Unlock()
		return own, nil
	}
	place := part.place(key)
	read, found := tx.reads[place]
	if !found {
		read = &txRead{part: part, key: key, done: make(chan struct{})}
		tx.reads[place] = read
		tx.order = append(tx.order, place)
		go func() {
			read.entry, read.err = fetch(context.WithoutCancel(ctx), part, key)
			close(read.done)
		}()
	}
	tx.mu.Unlock()
	select {
	case <-read.done:
		return read.entry, read.err
	case <-ctx.Done():
		return absent, ctx.Err()
	}
}

func fetch(ctx context.Context, part KvPart, key string) (wire.KvEntry, error) {
	var entry wire.KvEntry
	err := part.Link.Run(ctx, Reading, func(c *Connection) error {
		handle, err := HandleOn(ctx, c, part.Opener)
		if err != nil {
			return err
		}
		answer, err := c.Session.Call(ctx, wire.MethodKvGet, wire.Encode(&wire.KvCall{Handle: handle, Under: part.Under, Key: key}))
		if err != nil {
			return err
		}
		return wire.Decode(answer, &entry)
	})
	return entry, err
}

// own is the transaction's own word on a key: a write of it, or a clear of a
// branch above it, whichever came last.
func (tx *StoreTx) own(part KvPart, key string) (wire.KvEntry, bool) {
	written, wrote := tx.written[part.place(key)]
	cleared := 0
	for _, c := range tx.cleared {
		if c.handle == part.handlePlace() && len(part.Under) >= len(c.under) && equal(part.Under[:len(c.under)], c.under) {
			cleared = max(cleared, c.at)
		}
	}
	if wrote && written.at > cleared {
		return written.found, true
	}
	return absent, cleared > 0
}

func equal(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

// Write keeps a write for the commit; found is what a read of its key answers after it.
func (tx *StoreTx) Write(method uint16, part KvPart, call wire.KvCall, found *wire.KvEntry) error {
	tx.mu.Lock()
	defer tx.mu.Unlock()
	if err := tx.open(); err != nil {
		return err
	}
	tx.ops = append(tx.ops, txOp{method, part, call})
	if found != nil && method != wire.MethodKvClear {
		tx.written[part.place(call.Key)] = txWritten{*found, len(tx.ops)}
	}
	return nil
}

// Clear keeps a clear of a branch for the commit.
func (tx *StoreTx) Clear(part KvPart) error {
	if err := tx.Write(wire.MethodKvClear, part, wire.KvCall{}, nil); err != nil {
		return err
	}
	tx.mu.Lock()
	tx.cleared = append(tx.cleared, txCleared{part.handlePlace(), part.Under, len(tx.ops)})
	tx.mu.Unlock()
	return nil
}

// KeepJob keeps a write of a queue for the commit.
func (tx *StoreTx) KeepJob(op JobOp) error {
	tx.mu.Lock()
	defer tx.mu.Unlock()
	if err := tx.open(); err != nil {
		return err
	}
	tx.jobs = append(tx.jobs, op)
	return nil
}

// After keeps what the commit does once it answered: a message published
// inside is handed over then.
func (tx *StoreTx) After(fn func(ctx context.Context) error) error {
	tx.mu.Lock()
	defer tx.mu.Unlock()
	if err := tx.open(); err != nil {
		return err
	}
	tx.after = append(tx.after, fn)
	return nil
}

// Link is the link of the store the transaction is of.
func (tx *StoreTx) Link() *Link { return tx.store.Link }

func (tx *StoreTx) commit(ctx context.Context) error {
	tx.mu.Lock()
	tx.done = true
	tx.mu.Unlock()
	if err := tx.commitWrites(ctx); err != nil {
		return err
	}
	for _, fn := range tx.after {
		if err := fn(ctx); err != nil {
			return err
		}
	}
	return nil
}

func (tx *StoreTx) commitWrites(ctx context.Context) error {
	if len(tx.jobs) > 0 {
		return commitJobs(ctx, tx.store.Link, tx.jobs)
	}
	reads := make([]*txRead, 0, len(tx.order))
	for _, place := range tx.order {
		read := tx.reads[place]
		select {
		case <-read.done:
		case <-ctx.Done():
			return ctx.Err()
		}
		if read.err != nil {
			return read.err
		}
		reads = append(reads, read)
	}
	if len(reads) == 0 && len(tx.ops) == 0 {
		return nil
	}
	idempotence := Reading
	if len(tx.ops) > 0 {
		idempotence = Writing
	}
	return tx.store.Link.Run(ctx, idempotence, func(c *Connection) error {
		message := wire.KvTx{}
		for _, read := range reads {
			handle, err := HandleOn(ctx, c, read.part.Opener)
			if err != nil {
				return err
			}
			check := wire.KvCheck{Handle: handle, Under: read.part.Under, Key: read.key}
			if read.entry.Found {
				check.Version = read.entry.Version
				if check.Version == nil {
					check.Version = ptr([]byte{})
				}
			}
			message.Checks = append(message.Checks, check)
		}
		for _, op := range tx.ops {
			handle, err := HandleOn(ctx, c, op.part.Opener)
			if err != nil {
				return err
			}
			call := op.call
			call.Handle, call.Under = handle, op.part.Under
			message.Writes = append(message.Writes, wire.KvOp{Method: uint64(op.method), Call: call})
		}
		_, err := c.Session.Call(ctx, wire.MethodKvTx, wire.Encode(&message))
		return err
	})
}

// commitJobs sends a transaction's writes of jobs as one jobs.tx: all of them are made, or none.
func commitJobs(ctx context.Context, link *Link, ops []JobOp) error {
	return link.Run(ctx, Writing, func(c *Connection) error {
		message := wire.JobsTx{}
		for _, op := range ops {
			handle, err := HandleOn(ctx, c, op.Opener)
			if err != nil {
				return err
			}
			write := wire.JobsOp{Method: uint64(op.Method)}
			if op.Call != nil {
				call := *op.Call
				call.Handle = handle
				write.Call = &call
			} else {
				write.Id = &wire.JobsId{Handle: handle, Id: op.ID}
			}
			message.Writes = append(message.Writes, write)
		}
		_, err := c.Session.Call(ctx, wire.MethodJobsTx, wire.Encode(&message))
		return err
	})
}
