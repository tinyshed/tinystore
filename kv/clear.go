package kv

import (
	"context"
	"database/sql"
	"fmt"
	"slices"
	"strings"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// the owners below a bucket's root whose branches the test of a hidden row
// looks up by their prefixes' lengths, packed ten bits each into one integer:
// a path is at most 1 KiB, so a prefix that leaves a key room fits; the bit
// past them says that a branch lies deeper
const (
	hiddenLevels = 6
	lengthBits   = 10
	deeperBit    = 60
)

// hidden is true of a row a marked Clear hid: its version is the mark's or
// older, and the mark's prefix is a branch above it. Every statement that
// finds live rows says "and not" this, so a mark hides its rows from the
// moment it commits, to readers and writers alike.
//
// It asks first whether the row's bucket has a mark at all, which costs a
// bucket without one a single seek. Then it looks up the root and each branch
// above the row by its prefix, the first bytes of the row's path as many as
// the lengths packed in ?param say, so that a thousand marks cost a seek a
// branch rather than a thousand comparisons; one integer binds faster than a
// length each. A row deeper than the lengths reach is compared with every mark
// of its bucket instead: the prefix, then an owner's mark or a key's, since a
// name under another branch may begin with the prefix's bytes. row names the
// statement's table.
func hidden(row string, param int) string {
	lookups := []string{fmt.Sprintf(`exists (select 1 from branches as m where m.bucket = %[1]s.bucket
		and m.prefix = x'' and m.cleared >= %[1]s.version)`, row)}
	for level := range hiddenLevels {
		lookups = append(lookups, fmt.Sprintf(`exists (select 1 from branches as m where m.bucket = %[1]s.bucket
			and m.prefix = substr(%[1]s.path, 1, nullif((?%[2]d >> %[3]d) & %[4]d, 0))
			and m.cleared >= %[1]s.version)`, row, param, level*lengthBits, 1<<lengthBits-1))
	}
	lookups = append(lookups, fmt.Sprintf(`((?%[2]d >> %[3]d) & 1 and exists (select 1 from branches as m
			where m.bucket = %[1]s.bucket and m.cleared >= %[1]s.version
			and substr(%[1]s.path, 1, length(m.prefix)) = m.prefix
			and substr(%[1]s.path, length(m.prefix) + 1, 1) in (x'01', x'02')))`, row, param, deeperBit))
	return fmt.Sprintf(`(exists (select 1 from branches as g where g.bucket = %s.bucket) and (%s))`, row,
		strings.Join(lookups, " or "))
}

// Clear removes every key of this branch and of the branches under it. A
// branch of up to 10,000 keys is deleted in one transaction; a larger one is
// marked cleared at the file's revision, which hides its keys from every call
// at once, and Maintain deletes them 10,000 a transaction. A key written after
// the Clear is a new key. Inside Tx a branch over 10,000 keys is ErrLimit,
// since deleting it would hold the writer for seconds.
func (b *Bucket[V]) Clear(ctx context.Context) error {
	return b.clear(ctx)
}

// Clear removes every counter of this branch and of the branches under it, as
// Bucket.Clear does; LoseAtMost counters it removes from memory as well once
// it commits, so that no flush writes them again. One that fails leaves them.
func (c *Counters) Clear(ctx context.Context) error {
	switch {
	case c.memory == nil:
		return c.clear(ctx)
	case c.tx != nil:
		return c.fail(call{}, errRelaxedInTx)
	}
	return c.clearHeld(ctx)
}

const (
	countUnder = `select count(*) from (
		select 1 from cells where bucket = ?1 and path >= ?2 and path < ?3 limit cast(?4 as integer)
	)`
	deleteUnder = `delete from cells where bucket = ?1 and path >= ?2 and path < ?3 returning spill`
	markCleared = `insert into branches (bucket, prefix, cleared) values (?1, ?2, ?3)
		on conflict (bucket, prefix) do update set cleared = excluded.cleared`
)

// clear deletes the branch or marks it, in a transaction of its own or the
// handle's
func (b *branch) clear(ctx context.Context) error {
	if b.err != nil {
		return b.err
	}
	return b.fail(call{}, b.writeAlone(ctx, func(w sqlite.Writer) error { return b.clearIn(ctx, w) }))
}

// clearHeld clears LoseAtMost counters in a transaction of their own, which
// their memory commits beside
func (c *Counters) clearHeld(ctx context.Context) error {
	if c.err != nil {
		return c.err
	}
	release, err := c.state.admitWrite(ctx)
	if err != nil {
		return err
	}
	defer release()

	err = c.memory.clear(ctx, c.prefix, func(w sqlite.Writer) error { return c.clearIn(ctx, w) })
	return c.fail(call{}, err)
}

func (b *branch) clearIn(ctx context.Context, w sqlite.Writer) error {
	from, to := subtreeOf(b.prefix)
	var keys int
	err := sqlite.QueryRow(ctx, w, countUnder, b.id, from, to, b.state.clearBound+1).Scan(&keys)
	switch {
	case err != nil:
		return err
	case keys <= b.state.clearBound:
		return deleteRange(ctx, w, b.id, from, to)
	case b.tx != nil:
		return fmt.Errorf("%w: a Clear of more than %d keys inside a transaction", tinystore.ErrLimit,
			b.state.clearBound)
	}

	revision, err := b.state.nextRevision(ctx, w)
	if err == nil {
		_, err = w.ExecContext(ctx, markCleared, b.id, append([]byte{}, b.prefix...), revision)
	}
	return err
}

// writeAlone runs work in the handle's transaction, or in one of its own
// rather than a group's, since a Clear holds the writer for a while
func (b *branch) writeAlone(ctx context.Context, work func(sqlite.Writer) error) error {
	if b.tx != nil {
		writer, err := b.tx.writer(b.state)
		if err != nil {
			return err
		}
		return work(writer)
	}
	release, err := b.state.admitWrite(ctx)
	if err != nil {
		return err
	}
	defer release()
	return b.state.file.UpdatePrepared(ctx, work)
}

// subtreeOf is the range of paths under a branch, its own keys and its
// branches': its prefix, then an owner's mark or a key's
//
//	01 42 00        → [01 42 00 01, 01 42 00 03)
//	the bucket's root → [01, 03)
func subtreeOf(prefix []byte) (from, to []byte) {
	return append(slices.Clone(prefix), ownerMark), append(slices.Clone(prefix), keyMark+1)
}

// deleteRange deletes the rows between from and to and their spilled values
func deleteRange(ctx context.Context, w sqlite.Writer, bucket int64, from, to []byte) error {
	rows, err := w.QueryContext(ctx, deleteUnder, bucket, from, to)
	if err != nil {
		return err
	}
	spills, err := collectSpills(rows, "a cleared branch")
	for _, spill := range spills {
		if err == nil {
			err = dropSpilled(ctx, w, spill)
		}
	}
	return err
}

func collectSpills(rows *sql.Rows, what string) ([]sql.NullInt64, error) {
	var spills []sql.NullInt64
	err := sqlite.EachRow(rows, what, func(rows *sql.Rows) error {
		var spill sql.NullInt64
		if err := rows.Scan(&spill); err != nil {
			return err
		}
		spills = append(spills, spill)
		return nil
	})
	return spills, err
}

// mark is one marked Clear: under prefix, rows of version cleared or older
// are gone
type mark struct {
	bucket  int64
	prefix  []byte
	cleared int64
}

const (
	selectMarks = `select bucket, prefix, cleared from branches`
	dropHidden  = `delete from cells where (bucket, path) in (
		select bucket, path from cells where bucket = ?1 and path >= ?2 and path < ?3 and version <= ?4
		limit cast(?5 as integer)
	) returning spill`
	unmark = `delete from branches where bucket = ?1 and prefix = ?2 and cleared = ?3`
)

// dropCleared deletes the rows marked Clears hid, a bound's worth a
// transaction and at most ten transactions a mark, and each mark once nothing
// is left under it; it returns how many rows it deleted
func (s *Store) dropCleared(ctx context.Context) (int, error) {
	marks, err := s.readMarks(ctx)
	total := 0
	for _, marked := range marks {
		if err != nil {
			break
		}
		for range expiryBatches {
			var dropped int
			dropped, err = s.dropMarked(ctx, marked)
			total += dropped
			if err != nil || dropped < s.clearBound {
				break
			}
		}
	}
	return total, err
}

func (s *Store) readMarks(ctx context.Context) ([]mark, error) {
	var marks []mark
	err := s.file.View(ctx, func(tx *sql.Tx) error {
		rows, err := tx.QueryContext(ctx, selectMarks) //nolint:rowserrcheck // EachRow checks Err
		if err != nil {
			return err
		}
		return sqlite.EachRow(rows, "the marks of Clears", func(rows *sql.Rows) error {
			var marked mark
			if err := rows.Scan(&marked.bucket, &marked.prefix, &marked.cleared); err != nil {
				return err
			}
			marked.prefix = append([]byte{}, marked.prefix...)
			marks = append(marks, marked)
			return nil
		})
	})
	return marks, err
}

// dropMarked deletes one batch of the rows a mark hid, and the mark with the
// last of them
func (s *Store) dropMarked(ctx context.Context, marked mark) (int, error) {
	dropped := 0
	err := s.file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		from, to := subtreeOf(marked.prefix)
		rows, err := w.QueryContext(ctx, dropHidden, marked.bucket, from, to, marked.cleared, s.clearBound)
		if err != nil {
			return err
		}
		spills, err := collectSpills(rows, "rows a Clear hid")
		dropped = len(spills)
		for _, spill := range spills {
			if err == nil {
				err = dropSpilled(ctx, w, spill)
			}
		}
		if err == nil && dropped < s.clearBound {
			_, err = w.ExecContext(ctx, unmark, marked.bucket, marked.prefix, marked.cleared)
		}
		return err
	})
	return dropped, err
}
