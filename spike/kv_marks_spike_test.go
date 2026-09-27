package spike

import (
	"context"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// the owners above the measured key, as deep as a measurement asks
var kvMarkOwners = []string{"tenant-7", "42", "devices"}

// kvMarkVariant is one way a point Get skips what a marked Clear hid: the test
// it puts where kv puts its own, and the arguments it binds after the key's
const kvMarkRead = `select c.version, c.expires, c.value, c.spill from cells as c
	where c.bucket = ?1 and c.path = ?2 and (c.expires is null or c.expires > ?3)`

type kvMarkVariant struct {
	name  string
	test  string // empty for none
	bound func(ancestors [][]byte) []any
}

// TestKVClearMarks measures what a point Get pays to skip the rows marked
// Clears hid. kv tests every mark of the bucket by a substring of the path;
// the others look up the Get's own branch and the ones above it: an IN list
// padded to eight with nulls or as long as the branch is deep, eight EXISTS
// of one prefix each, and a JSON array of the prefixes in hex; beside them the
// statement with no test at all. The marks name other branches, cleared after
// the key was written, so that each passes the version test: the case the
// substring pays most for. Each Get must find the key, and none may once its
// own branch is marked.
func TestKVClearMarks(t *testing.T) {
	kvMeasuring(t)
	for _, depth := range []int{1, 3} {
		for _, marks := range []int{0, 10, 100, 1000} {
			file := kvMarksFile(t, depth, marks)
			variants := kvMarkVariants(depth)
			for _, variant := range variants {
				kvCheckMarkVariant(t, file, variant, depth)
			}
			times := map[string][]time.Duration{}
			for range 3 {
				for _, variant := range variants {
					perGet, err := kvTimeMarkVariant(t.Context(), file, variant, depth, kvSeconds()/3)
					if err != nil {
						t.Fatalf("%s: %v", variant.name, err)
					}
					times[variant.name] = append(times[variant.name], perGet)
				}
			}
			for _, variant := range variants {
				slices.Sort(times[variant.name])
				t.Logf("depth=%d marks=%d variant=%s get=%v runs=%v", depth, marks, variant.name,
					times[variant.name][1], times[variant.name])
			}
		}
	}
}

// kvMarksFile is a file of one key under depth owners, and marks of as many
// other branches at the root, each cleared after the key was written
func kvMarksFile(t *testing.T, depth, marks int) *sqlite.File {
	t.Helper()
	file := kvOpen(t, filepath.Join(kvDir(t), "kv.db"), 4096, kvReaders)
	err := file.Update(t.Context(), func(tx *sql.Tx) error {
		if _, err := tx.ExecContext(t.Context(), `create table branches (
			bucket integer not null, prefix blob not null, cleared integer not null,
			primary key (bucket, prefix)) without rowid`); err != nil {
			return err
		}
		if _, err := tx.ExecContext(t.Context(), kvUpsert, 1, kvPath("key", kvMarkOwners[:depth]...), 1, nil,
			kvValue(0, 64)); err != nil {
			return err
		}
		for i := range marks {
			other := kvOwnerPrefix(fmt.Sprintf("cleared-%06d", i))
			if _, err := tx.ExecContext(t.Context(), `insert into branches values (1, ?1, 2)`, other); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	return file
}

// kvOwnerPrefix is the prefix of the branch owners name, as kvPath writes it
//
//	"tenant-7", "42" → 01 tenant-7 00 01 42 00
func kvOwnerPrefix(owners ...string) []byte {
	var prefix []byte
	for _, owner := range owners {
		prefix = append(kvEscape(append(prefix, 0x01), owner), 0x00)
	}
	return prefix
}

// kvAncestors are the branches a key under depth owners lies in: the bucket's
// root, then each owner's
func kvAncestors(depth int) [][]byte {
	ancestors := [][]byte{{}}
	for level := 1; level <= depth; level++ {
		ancestors = append(ancestors, kvOwnerPrefix(kvMarkOwners[:level]...))
	}
	return ancestors
}

func kvMarkVariants(depth int) []kvMarkVariant {
	const substring = `exists (select 1 from branches as m where m.bucket = c.bucket and m.cleared >= c.version
		and substr(c.path, 1, length(m.prefix)) = m.prefix
		and substr(c.path, length(m.prefix) + 1, 1) in (x'01', x'02'))`
	lookups := func(count int) string {
		var tests []string
		for i := range count {
			tests = append(tests, fmt.Sprintf(`exists (select 1 from branches as m where m.bucket = c.bucket
				and m.prefix = ?%d and m.cleared >= c.version)`, 4+i))
		}
		return "(" + strings.Join(tests, " or ") + ")"
	}
	padded := func(count int) func([][]byte) []any {
		return func(ancestors [][]byte) []any {
			bound := make([]any, count)
			for i, ancestor := range ancestors[:min(len(ancestors), count)] {
				bound[i] = ancestor
			}
			return bound
		}
	}
	return []kvMarkVariant{
		{name: "none"},
		{name: "substring", test: substring, bound: padded(0)},
		{name: "in-8", test: kvInList(8), bound: padded(8)},
		{name: "in-depth", test: kvInList(depth + 1), bound: padded(depth + 1)},
		{name: "exists-8", test: lookups(8), bound: padded(8)},
		{
			name: "json", test: `exists (select 1 from branches as m where m.bucket = c.bucket
			and m.prefix in (select unhex(value) from json_each(?4)) and m.cleared >= c.version)`,
			bound: kvJSONPrefixes,
		},
		{name: "guarded-in-8", test: kvGuarded + kvInList(8) + ")", bound: padded(8)},
		{name: "guarded-in-depth", test: kvGuarded + kvInList(depth+1) + ")", bound: padded(depth + 1)},
		{
			name: "guarded-json", test: kvGuarded + `exists (select 1 from branches as m where m.bucket = c.bucket
			and m.prefix in (select unhex(value) from json_each(?4)) and m.cleared >= c.version))`,
			bound: kvJSONPrefixes,
		},
	}
}

// kvGuarded looks for a mark of the bucket before it looks up the branches, so
// that a bucket without one pays a single seek, as the substring does
const kvGuarded = `(exists (select 1 from branches as g where g.bucket = c.bucket) and `

func kvInList(count int) string {
	var placeholders []string
	for i := range count {
		placeholders = append(placeholders, fmt.Sprintf("?%d", 4+i))
	}
	return `exists (select 1 from branches as m where m.bucket = c.bucket
		and m.prefix in (` + strings.Join(placeholders, ", ") + `) and m.cleared >= c.version)`
}

func kvJSONPrefixes(ancestors [][]byte) []any {
	spelled := make([]string, len(ancestors))
	for i, ancestor := range ancestors {
		spelled[i] = hex.EncodeToString(ancestor)
	}
	encoded, err := json.Marshal(spelled)
	if err != nil {
		panic(err)
	}
	return []any{string(encoded)}
}

// kvMarkGet reads the key through variant and says whether it is there
func kvMarkGet(ctx context.Context, file *sqlite.File, variant kvMarkVariant, depth int) (bool, error) {
	query, arguments := kvMarkRead, []any{1, kvPath("key", kvMarkOwners[:depth]...), time.Now().UnixMilli()}
	if variant.test != "" {
		query += " and not " + variant.test
		arguments = append(arguments, variant.bound(kvAncestors(depth))...)
	}
	found := false
	err := file.Lookup(ctx, func(r sqlite.Reader) error {
		var version int64
		var expires, spill sql.NullInt64
		var value any
		err := sqlite.QueryRow(ctx, r, query, arguments...).Scan(&version, &expires, &value, &spill)
		if errors.Is(err, sql.ErrNoRows) {
			return nil
		}
		found = err == nil
		return err
	})
	return found, err
}

// kvCheckMarkVariant holds a variant to the rule: the key is there beside the
// marks of other branches, and gone while its own branch, or the root, is
// marked after it was written
func kvCheckMarkVariant(t *testing.T, file *sqlite.File, variant kvMarkVariant, depth int) {
	t.Helper()
	if found, err := kvMarkGet(t.Context(), file, variant, depth); !found || err != nil {
		t.Fatalf("%s: the key beside other branches' marks: %v, %v", variant.name, found, err)
	}
	if variant.test == "" {
		return
	}
	for _, ancestor := range kvAncestors(depth) {
		mark := func(statement string) {
			err := file.Update(t.Context(), func(tx *sql.Tx) error {
				_, err := tx.ExecContext(t.Context(), statement, ancestor)
				return err
			})
			if err != nil {
				t.Fatal(err)
			}
		}
		mark(`insert into branches values (1, ?1, 2)`)
		found, err := kvMarkGet(t.Context(), file, variant, depth)
		mark(`delete from branches where bucket = 1 and prefix = ?1`)
		if found || err != nil {
			t.Fatalf("%s: the key under the marked branch %x: %v, %v", variant.name, ancestor, found, err)
		}
	}
}

// kvTimeMarkVariant reads the key through variant, one Get after another, for
// length, and returns what one took
func kvTimeMarkVariant(ctx context.Context, file *sqlite.File, variant kvMarkVariant, depth int,
	length time.Duration,
) (time.Duration, error) {
	gets := 0
	began := time.Now()
	for time.Since(began) < length {
		if _, err := kvMarkGet(ctx, file, variant, depth); err != nil {
			return 0, err
		}
		gets++
	}
	return time.Since(began) / time.Duration(gets), nil
}
