package spike

import (
	"database/sql"
	"encoding/binary"
	"fmt"
	"math/rand/v2"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// sqldbUUIDv7 is the i-th of a run of version 7 uuids: the milliseconds of its
// making first, so that ids made later sort later
func sqldbUUIDv7(i int) [16]byte {
	var id [16]byte
	binary.BigEndian.PutUint64(id[:8], uint64(sqldbEpoch+int64(i))<<16)
	random := rand.New(rand.NewPCG(uint64(i), 29))
	binary.LittleEndian.PutUint64(id[8:], random.Uint64())
	id[6] = 0x70 | id[6]&0x0f
	id[7] = byte(random.Uint32())
	id[8] = id[8]&0x3f | 0x80
	return id
}

// TestSQLDBUUIDKeys writes 500,000 notes keyed by a uuid and a comment on each
// that references its note through an index, four times: the uuid as its
// text and as 16 bytes, of version 4, random, and of version 7, ordered by
// time. It reports what the writes took, what each object of the file holds,
// and a note's and its comments' reads by key
func TestSQLDBUUIDKeys(t *testing.T) {
	sqldbMeasuring(t)
	const count = 500_000
	for _, variant := range []struct {
		name    string
		storage string
		make    func(int) [16]byte
	}{
		{"text, version 4", "text", sqldbUUID},
		{"text, version 7", "text", sqldbUUIDv7},
		{"blob, version 4", "blob", sqldbUUID},
		{"blob, version 7", "blob", sqldbUUIDv7},
	} {
		key := func(i int) any {
			id := variant.make(i)
			if variant.storage == "text" {
				return sqldbUUIDText(id)
			}
			return id[:]
		}
		path := filepath.Join(sqldbDir(t), "app.db")
		file := sqldbOpen(t, path, 1)
		ctx := t.Context()
		schema := strings.NewReplacer("<key>", variant.storage).Replace(`
			create table notes (id <key> not null primary key, author_id integer not null, title text not null) strict;
			create table comments (
				id      integer primary key,
				note_id <key> not null references notes (id),
				body    text not null
			) strict;
			create index comments_note_id on comments (note_id);`)
		if err := file.Update(ctx, func(tx *sql.Tx) error {
			_, err := tx.ExecContext(ctx, schema)
			return err
		}); err != nil {
			t.Fatal(err)
		}

		insert := func(query string, args func(i int) []any) time.Duration {
			began := time.Now()
			for first := 0; first < count; first += 10_000 {
				err := file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
					for i := first; i < first+10_000; i++ {
						if _, err := w.ExecContext(ctx, query, args(i)...); err != nil {
							return err
						}
					}
					return nil
				})
				if err != nil {
					t.Fatal(err)
				}
			}
			return time.Since(began)
		}
		notes := insert(`insert into notes (id, author_id, title) values (?, ?, ?)`, func(i int) []any {
			return []any{key(i), i % sqldbAuthors, "a note, number " + fmt.Sprint(i)}
		})
		comments := insert(`insert into comments (note_id, body) values (?, ?)`, func(i int) []any {
			return []any{key((i * 7919) % count), "seen"}
		})
		t.Logf("%-16s %d notes in %v, %d comments in %v", variant.name, count, notes.Round(time.Millisecond),
			count, comments.Round(time.Millisecond))

		if err := file.View(ctx, func(tx *sql.Tx) error {
			//nolint:rowserrcheck // EachRow checks Err
			rows, err := tx.QueryContext(ctx, `select name, sum(pgsize) from dbstat group by name order by name`)
			if err != nil {
				return err
			}
			return sqlite.EachRow(rows, "dbstat", func(rows *sql.Rows) error {
				var name string
				var size int64
				if err := rows.Scan(&name, &size); err != nil {
					return err
				}
				if !strings.HasPrefix(name, "sqlite_schema") {
					t.Logf("%-16s   %-26s %6.1f MiB  %5.1f bytes a row", variant.name, name, float64(size)/(1<<20),
						float64(size)/count)
				}
				return nil
			})
		}); err != nil {
			t.Fatal(err)
		}

		for _, read := range []struct{ name, query string }{
			{"a note by its id", `select id, author_id, title from notes where id = ?`},
			{"a note's comments", `select id, body from comments where note_id = ?`},
		} {
			random := rand.New(rand.NewPCG(13, 31))
			result := kvLoad(1, sqldbSeconds(), func(int) error {
				return file.Lookup(ctx, func(r sqlite.Reader) error {
					//nolint:rowserrcheck // EachRow checks Err
					rows, err := r.QueryContext(ctx, read.query, key(random.IntN(count)))
					if err != nil {
						return err
					}
					found := 0
					err = sqlite.EachRow(rows, "a read", func(*sql.Rows) error { found++; return nil })
					if err == nil && found == 0 && read.name == "a note by its id" {
						return errKVWrongAnswer
					}
					return err
				})
			})
			t.Logf("%-16s   %-20s %s", variant.name, read.name, result)
		}
		if err := file.Close(); err != nil {
			t.Fatal(err)
		}
	}
}
