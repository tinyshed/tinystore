package spike

import (
	"bytes"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"math/rand/v2"
	"path/filepath"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// a layout measurement's population: small enough for twenty-two files, large
// enough for trees three levels deep
const kvLayoutRows = 20_000

// TestKVLayout divides kv.db by object with dbstat: sessions at 1 and 4 KiB
// pages, values of 16 to 4096 bytes kept in the row or, from 512 bytes,
// spilled to a table of their own; and times a lookup that needs no value,
// which is what Has, SetIfAbsent's check and the expiry sweep pay
func TestKVLayout(t *testing.T) {
	kvMeasuring(t)
	for _, pageSize := range []int{1024, 4096} {
		for _, size := range []int{16, 64, 256, 512, 768, 1024, 4096} {
			for _, spill := range []bool{false, true} {
				if spill && size < 512 {
					continue
				}
				path := filepath.Join(kvDir(t), "kv.db")
				file := kvOpen(t, path, pageSize, 2)
				kvWriteLayout(t, file, size, spill)
				objects := kvObjects(t, file)
				has := kvTimeHas(t, path)
				where := "in the row"
				if spill {
					where = "spilled"
				}
				t.Logf("page %4d  value %4d B %-10s  %s  Has %s", pageSize, size, where, objects, has)
			}
		}
	}
}

// kvWriteLayout writes kvLayoutRows sessions in a scattered order in one
// transaction, a spilled value as a row of spilled that the cell names
func kvWriteLayout(t *testing.T, file *sqlite.File, size int, spill bool) {
	t.Helper()
	ctx := t.Context()
	step := kvStride(kvLayoutRows)
	err := file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
		for n := range kvLayoutRows {
			i := n * step % kvLayoutRows
			if err := kvWriteSession(ctx, w, i, kvValue(i, size), spill); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}

func kvWriteSession(ctx context.Context, w sqlite.Writer, i int, value []byte, spill bool) error {
	path := kvSession(kvSeedStored, i)
	if !spill {
		_, err := w.ExecContext(ctx, kvUpsert, 1, path, i+1, kvExpiry(i), value)
		return err
	}
	result, err := w.ExecContext(ctx, `insert into spilled (value) values (?1)`, value)
	if err != nil {
		return err
	}
	id, err := result.LastInsertId()
	if err != nil {
		return err
	}
	_, err = w.ExecContext(ctx, `insert into cells (bucket, path, version, expires, spill) values (1, ?1, ?2, ?3, ?4)`,
		path, i+1, kvExpiry(i), id)
	return err
}

// kvObjectBytes is one b-tree's pages: all of them, those holding a row's
// remainder, and what they leave unused
type kvObjectBytes struct {
	total, overflow, unused int64
}

// kvLayoutObjects is the file divided by the objects that matter
type kvLayoutObjects struct {
	cells, expiry, spilled, file kvObjectBytes
}

func (o kvLayoutObjects) String() string {
	perRow := func(bytes int64) float64 { return float64(bytes) / kvLayoutRows }
	unused := func(b kvObjectBytes) float64 { return 100 * float64(b.unused) / float64(max(b.total, 1)) }
	return fmt.Sprintf("B/row: cells %7.1f (%4.1f%% overflow, %4.1f%% unused)  expiry index %5.1f  spilled %7.1f  file %7.1f",
		perRow(o.cells.total), 100*float64(o.cells.overflow)/float64(max(o.cells.total, 1)), unused(o.cells),
		perRow(o.expiry.total), perRow(o.spilled.total), perRow(o.file.total))
}

// kvObjects reads dbstat, which counts every b-tree's own pages, so that a
// saving says which object it came from
func kvObjects(t *testing.T, file *sqlite.File) kvLayoutObjects {
	t.Helper()
	ctx := t.Context()
	var objects kvLayoutObjects
	err := file.View(ctx, func(tx *sql.Tx) error {
		//nolint:rowserrcheck // EachRow checks Err
		rows, err := tx.QueryContext(ctx, `select name, pagetype, sum(pgsize), sum(unused) from dbstat group by name, pagetype`)
		if err != nil {
			return err
		}
		return sqlite.EachRow(rows, "dbstat", func(rows *sql.Rows) error {
			var (
				name, kind    string
				pages, unused int64
			)
			if err := rows.Scan(&name, &kind, &pages, &unused); err != nil {
				return err
			}
			var object *kvObjectBytes
			switch name {
			case "cells":
				object = &objects.cells
			case "cells_expiry":
				object = &objects.expiry
			case "spilled":
				object = &objects.spilled
			}
			for _, into := range []*kvObjectBytes{object, &objects.file} {
				if into == nil {
					continue
				}
				into.total += pages
				into.unused += unused
				if kind == "overflow" {
					into.overflow += pages
				}
			}
			return nil
		})
	})
	if err != nil {
		t.Fatal(err)
	}
	return objects
}

// kvTimeHas times a lookup of stored keys that reads their rows and not their
// values
func kvTimeHas(t *testing.T, path string) kvResult {
	t.Helper()
	ctx := t.Context()
	statement, err := kvReaderPool(t, path, "cache_size(-1024)").PrepareContext(ctx,
		`select 1 from cells where bucket = 1 and path = ?1`)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = statement.Close() })
	random := rand.New(rand.NewPCG(kvLayoutRows, 17))
	result := kvLoad(1, time.Second, func(int) error {
		var one int
		return statement.QueryRowContext(ctx, kvSession(kvSeedStored, random.IntN(kvLayoutRows))).Scan(&one)
	})
	if result.failed > 0 {
		t.Fatal(result.firstErr)
	}
	return result
}

// TestKVClear times Clear on a branch of 1,000 to 1,000,000 keys among 200,000
// sessions: one delete over the branch's range, its expiry index included, in
// one transaction, which is how long the writer is held
func TestKVClear(t *testing.T) {
	kvMeasuring(t)
	for _, size := range []int{1_000, 10_000, 100_000, 1_000_000} {
		file := kvOpen(t, filepath.Join(kvDir(t), "kv.db"), 4096, 2)
		kvPreload(t, file, 200_000, 64)
		kvWriteBranch(t, file, "branch", size)

		prefix := []byte{0x01, 'b', 'r', 'a', 'n', 'c', 'h', 0x00}
		end := bytes.Clone(prefix)
		end[len(end)-1]++

		began := time.Now()
		err := file.UpdatePrepared(t.Context(), func(w sqlite.Writer) error {
			result, err := w.ExecContext(t.Context(), `delete from cells where bucket = 1 and path >= ?1 and path < ?2`, prefix, end)
			if err != nil {
				return err
			}
			if cleared, err := result.RowsAffected(); err != nil || cleared != int64(size) {
				return errors.Join(err, fmt.Errorf("cleared %d keys of %d", cleared, size))
			}
			return nil
		})
		took := time.Since(began)
		if err != nil {
			t.Fatal(err)
		}
		t.Logf("%8d keys cleared in one transaction  %9.1f ms  %6.0f keys/ms", size,
			float64(took)/float64(time.Millisecond), float64(size)/(float64(took)/float64(time.Millisecond)))
	}
}

// kvWriteBranch writes size keys under one owner, fifty thousand a transaction
func kvWriteBranch(t *testing.T, file *sqlite.File, owner string, size int) {
	t.Helper()
	ctx := t.Context()
	for start := 0; start < size; start += 50_000 {
		end := min(start+50_000, size)
		err := file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
			for i := start; i < end; i++ {
				path := kvPath(fmt.Sprintf("%016x", uint64(i)*0x9e3779b97f4a7c15), owner)
				if _, err := w.ExecContext(ctx, kvUpsert, 1, path, i+1, kvExpiry(i), kvValue(i, 64)); err != nil {
					return err
				}
			}
			return nil
		})
		if err != nil {
			t.Fatal(err)
		}
	}
}
