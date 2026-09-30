package sqldb

import (
	"context"
	"fmt"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Batch is statements gathered before any of them runs, which then commit
// together or not at all.
//
// Nothing runs while the batch is built, so building it may take as long as it
// likes; what it cannot do is read what an earlier statement of it wrote. A
// statement that depends on an earlier one says so in SQL, with
// last_insert_rowid() or a key the program chose; one that needs to read first
// and decide belongs in a Tx.
type Batch struct {
	calls []*call
}

// Exec adds a statement to the batch; it runs when the batch does.
func (b *Batch) Exec(query string, args ...any) {
	b.calls = append(b.calls, &call{query: query, args: args, write: true})
}

// Batch runs the statements build adds in one savepoint of a grouped commit, as
// an Exec runs its one: they share an fsync with the writes beside them, and a
// batch whose statement fails rolls back alone. An error from build, or an
// argument no column can hold, writes nothing.
//
// A Tx holds the writer for itself and pays a commit of its own; a batch, whose
// statements are known before it runs, need not.
func (d *DB) Batch(ctx context.Context, build func(*Batch) error) error {
	var b Batch
	if err := build(&b); err != nil {
		return err
	}
	if len(b.calls) == 0 {
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
	err = d.file.UpdateGroupedAs(ctx, b.calls[0].query, weight, func(w sqlite.Writer) (err error) {
		defer func() {
			if recovered := recover(); recovered != nil {
				panicked, err = recovered, errPanicked
			}
		}()
		return b.writeTo(ctx, sqlite.UntilDeadline(w), args, held)
	})
	if panicked != nil {
		panic(panicked)
	}
	return d.explain(err)
}

// arguments encodes every statement's arguments before any runs, and weighs
// them against a group's bound
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
	return args, weight, nil
}

func (b *Batch) writeTo(ctx context.Context, w sqlite.Writer, args [][]any, held *held) error {
	for i, c := range b.calls {
		if err := c.writeTo(ctx, w, args[i], held); err != nil {
			return fmt.Errorf("statement %d of the batch: %w", i+1, err)
		}
	}
	return nil
}
