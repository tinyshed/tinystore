package sqldb

import (
	"context"
	"fmt"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Batch is writes gathered before any of them runs, which then commit together
// or not at all: statements, and what another engine keeps in this database,
// such as a job that a jobs store opened with Options.In enqueues.
//
// Nothing runs while the batch is built, so building it may take as long as it
// likes; what it cannot do is read what an earlier write of it wrote. A
// statement that depends on an earlier one says so in SQL, with
// last_insert_rowid() or a key the program chose; one that needs to read first
// and decide belongs in a Tx.
type Batch struct {
	calls   []*call
	changes []Change
}

// Change is a write another engine keeps in this database, which a Batch runs
// beside its statements, in their savepoint. A program does not implement it:
// its methods name the store's own types. An engine gives one out, as a jobs
// queue's Enqueued does.
type Change interface {
	// Bytes is what the change holds, counted against the group's bound, or
	// why it cannot be written, which writes nothing of the batch.
	Bytes() (int, error)
	// Apply writes the change with the batch's writer. file is the database's,
	// which the change refuses when its tables are in another.
	Apply(ctx context.Context, file *sqlite.File, w sqlite.Writer) error
	// Done is told the batch's outcome once, nil when it is durable, whatever
	// became of it, so that the change lets go of what it holds.
	Done(err error)
}

// Exec adds a statement to the batch; it runs when the batch does.
func (b *Batch) Exec(query string, args ...any) {
	b.calls = append(b.calls, &call{query: query, args: args, write: true})
}

// Add adds another engine's write to the batch; it runs after the statements.
func (b *Batch) Add(change Change) {
	b.changes = append(b.changes, change)
}

// Batch runs what build adds in one savepoint of a grouped commit, as an Exec
// runs its one statement: they share an fsync with the writes beside them, and
// a batch whose statement or change fails rolls back alone. An error from
// build, an argument no column can hold or a change that cannot be written
// writes nothing.
//
// A Tx holds the writer for itself and pays a commit of its own; a batch, whose
// writes are known before it runs, need not.
func (d *DB) Batch(ctx context.Context, build func(*Batch) error) (err error) {
	var b Batch
	defer func() { b.done(err) }()
	if err = b.build(build); err != nil {
		return err
	}
	if len(b.calls) == 0 && len(b.changes) == 0 {
		return nil
	}
	args, weight, err := b.arguments()
	if err != nil {
		return d.explain(err)
	}

	leave, err := d.admitWrite(ctx)
	if err != nil {
		return err
	}
	defer leave()
	held, err := d.hold(ctx, int64(weight), 0, true)
	if err != nil {
		return err
	}
	defer held.release()
	held.used = int64(weight)

	var panicked any
	err = d.file.UpdateGroupedAs(ctx, b.label(), weight, func(w sqlite.Writer) (err error) {
		defer func() {
			if recovered := recover(); recovered != nil {
				panicked, err = recovered, errPanicked
			}
		}()
		return b.writeTo(ctx, d.file, w, args, held)
	})
	if panicked != nil {
		b.done(errPanicked)
		b.changes = nil
		panic(panicked)
	}
	return d.explain(err)
}

// build runs the program's build, telling the changes it added that nothing
// will run when it panics
func (b *Batch) build(build func(*Batch) error) error {
	built := false
	defer func() {
		if !built {
			b.done(errPanicked)
			b.changes = nil
		}
	}()
	err := build(b)
	built = true
	return err
}

// arguments encodes every statement's arguments and weighs them and the
// changes before any runs
func (b *Batch) arguments() ([][]any, int, error) {
	args := make([][]any, len(b.calls))
	weight := 0
	for i, c := range b.calls {
		encoded, err := c.arguments()
		if err != nil {
			return nil, 0, fmt.Errorf("statement %d of the batch: %w", i+1, err)
		}
		args[i] = encoded
		weight += weigh(encoded)
	}
	for i, change := range b.changes {
		bytes, err := change.Bytes()
		if err != nil {
			return nil, 0, fmt.Errorf("change %d of the batch: %w", i+1, err)
		}
		weight += bytes
	}
	return args, weight, nil
}

// writeTo runs the statements until their callers' deadlines, as an Exec runs
// its one, and the changes without them, as an engine's own writes run
func (b *Batch) writeTo(ctx context.Context, file *sqlite.File, w sqlite.Writer, args [][]any, held *held) error {
	for i, c := range b.calls {
		if err := c.writeTo(ctx, sqlite.UntilDeadline(w), args[i], held); err != nil {
			return fmt.Errorf("statement %d of the batch: %w", i+1, err)
		}
	}
	for i, change := range b.changes {
		if err := change.Apply(ctx, file, w); err != nil {
			return fmt.Errorf("change %d of the batch: %w", i+1, err)
		}
	}
	return nil
}

// done tells each change the batch's outcome, once
func (b *Batch) done(err error) {
	for _, change := range b.changes {
		change.Done(err)
	}
	b.changes = nil
}

// label names the batch to Config.Waited: its first statement, or its changes
func (b *Batch) label() string {
	if len(b.calls) > 0 {
		return b.calls[0].query
	}
	return "a batch of changes"
}
