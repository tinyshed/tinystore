package dbstat

import (
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// written is a closed file whose b-trees have interior pages and overflow
// chains, and a freelist, in pages small enough that a few thousand rows make
// them, with what SQLite counted of it before it closed
type written struct {
	path                  string
	pages, free, pageSize int64
}

func write(t *testing.T, vacuum string) written {
	t.Helper()
	path := filepath.Join(t.TempDir(), "stat.db")
	db, err := sqlite.OpenDB("file:" + filepath.ToSlash(path))
	if err != nil {
		t.Fatal(err)
	}
	db.SetMaxOpenConns(1)
	statements := []string{
		"pragma page_size = 1024",
		"pragma auto_vacuum = " + vacuum,
		"pragma journal_mode = wal",
		"pragma synchronous = off", // a file to count, not to keep
		"create table notes (id integer primary key, title text not null)",
		"create index notes_title on notes (title)",
		"create table files (id integer primary key, data blob not null)",
		"create table words (word text primary key, seen integer) without rowid",
		"create table empty (id integer primary key)",
	}
	for i := range 3000 {
		statements = append(statements,
			fmt.Sprintf("insert into notes (title) values ('note %05d %s')", i, strings.Repeat("x", i%200)))
	}
	for i := range 500 {
		statements = append(statements,
			fmt.Sprintf("insert into words values ('%s', %d)", strings.Repeat(fmt.Sprintf("%04d", i), 100), i))
	}
	statements = append(statements,
		"insert into files (data) select randomblob(5000) from generate_series(1, 20)",
		"delete from notes where id % 2 = 0",
	)
	for _, statement := range statements {
		if _, err = db.ExecContext(t.Context(), statement); err != nil {
			t.Fatalf("%s: %v", statement, err)
		}
	}
	w := written{path: path}
	for pragma, into := range map[string]*int64{"page_count": &w.pages, "freelist_count": &w.free, "page_size": &w.pageSize} {
		if err = db.QueryRowContext(t.Context(), "pragma "+pragma).Scan(into); err != nil {
			t.Fatal(err)
		}
	}
	if err = db.Close(); err != nil {
		t.Fatal(err)
	}
	return w
}

// Every page of a file is an object's, the freelist's or a pointer map's,
// and the objects hold what they were given: an empty table one page, twenty
// blobs of 5000 bytes four overflow pages each and a leaf.
func TestEveryPageIsCountedOnce(t *testing.T) {
	for _, vacuum := range []string{"none", "incremental"} {
		w := write(t, vacuum)
		objects, err := Read(t.Context(), w.path)
		if err != nil {
			t.Fatalf("auto_vacuum %s: %v", vacuum, err)
		}
		pages := map[string]int64{}
		var names []string
		var total int64
		for _, o := range objects {
			pages[o.Name], total = o.Pages, total+o.Pages
			names = append(names, o.Name)
			if o.Bytes != o.Pages*w.pageSize {
				t.Errorf("auto_vacuum %s: %s is %d pages and %d bytes", vacuum, o.Name, o.Pages, o.Bytes)
			}
		}
		maps := int64(0)
		if vacuum != "none" {
			maps = (w.pages-2)/(w.pageSize/5+1) + 1
		}
		if total+w.free+maps != w.pages {
			t.Errorf("auto_vacuum %s: %d pages in objects, %d free and %d maps of the file's %d",
				vacuum, total, w.free, maps, w.pages)
		}
		if strings.Join(names, " ") != "sqlite_schema notes notes_title files words empty" ||
			pages["empty"] != 1 || pages["files"] < 20*5 || pages["words"] < 500 {
			t.Errorf("auto_vacuum %s: the objects are %v", vacuum, pages)
		}
	}
}

// A page reached twice, or one nothing reaches, fails the division rather than
// shift its bytes to another object.
func TestAPageCountedTwiceOrNeverIsRefused(t *testing.T) {
	for _, c := range []struct {
		name  string
		trunk uint32
		want  string
	}{
		{"the freelist starting at the schema", 1, "page 1 is counted twice"},
		{"the freelist forgotten", 0, "belong to nothing the walk found"},
	} {
		w := write(t, "none")
		file, err := os.OpenFile(w.path, os.O_RDWR, 0)
		if err != nil {
			t.Fatal(err)
		}
		var trunk [4]byte
		binary.BigEndian.PutUint32(trunk[:], c.trunk)
		_, err = file.WriteAt(trunk[:], 32)
		if err = errors.Join(err, file.Close()); err != nil {
			t.Fatal(err)
		}
		if _, err = Read(t.Context(), w.path); err == nil || !strings.Contains(err.Error(), c.want) {
			t.Errorf("%s: %v", c.name, err)
		}
	}
}

func TestADamagedHeaderOrCellIsRefused(t *testing.T) {
	w := write(t, "none")
	for _, c := range []struct {
		name   string
		offset int64
		bytes  []byte
	}{
		{"zero page size", 16, []byte{0, 0}},
		{"a page size that is not a power of two", 16, []byte{3, 0}},
		{"too many pages", 28, []byte{255, 255, 255, 255}},
		{"invalid payload fractions", 21, []byte{0}},
		{"unknown text encoding", 56, []byte{0, 0, 0, 4}},
		{"too many schema cells", 103, []byte{255, 255}},
		{"a schema cell over its header", 108, []byte{0, 0}},
	} {
		t.Run(c.name, func(t *testing.T) {
			f, err := os.OpenFile(w.path, os.O_RDWR, 0)
			if err != nil {
				t.Fatal(err)
			}
			original := make([]byte, len(c.bytes))
			if _, err = f.ReadAt(original, c.offset); err != nil {
				t.Fatal(err)
			}
			if _, err = f.WriteAt(c.bytes, c.offset); err != nil {
				t.Fatal(err)
			}
			_, readErr := Read(t.Context(), w.path)
			_, err = f.WriteAt(original, c.offset)
			if err = errors.Join(err, f.Close()); err != nil {
				t.Fatal(err)
			}
			if readErr == nil {
				t.Fatal("the damaged file was counted")
			}
		})
	}
}

func TestASchemasNamesKeepTheFilesEncodingAndPageSize(t *testing.T) {
	for _, encoding := range []string{"UTF-8", "UTF-16le", "UTF-16be"} {
		for _, size := range []int{512, 65536} {
			t.Run(fmt.Sprintf("%s/%d", encoding, size), func(t *testing.T) {
				path := filepath.Join(t.TempDir(), "stat.db")
				db, err := sqlite.OpenDB("file:" + filepath.ToSlash(path))
				if err != nil {
					t.Fatal(err)
				}
				_, err = db.ExecContext(t.Context(), fmt.Sprintf(`pragma page_size = %d;
					pragma encoding = '%s'; create table "notes" (value text);`, size, encoding))
				if err = errors.Join(err, db.Close()); err != nil {
					t.Fatal(err)
				}
				objects, err := Read(t.Context(), path)
				if err != nil || len(objects) != 2 || objects[1].Name != "notes" || objects[1].Bytes != int64(size) {
					t.Fatalf("objects %+v, %v", objects, err)
				}
			})
		}
	}
}

func TestACancelledReadOpensNothing(t *testing.T) {
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err := Read(ctx, "does-not-exist.db"); !errors.Is(err, context.Canceled) {
		t.Fatalf("a cancelled read: %v", err)
	}
}

func FuzzSchemaRows(f *testing.F) {
	f.Add([]byte{6, 23, 23, 23, 1, 0, 't', 'a', 'b', 'l', 'e', 'n', 'o', 't', 'e', 's', 'n', 'o', 't', 'e', 's', 2})
	f.Add([]byte{128})
	f.Fuzz(func(t *testing.T, record []byte) {
		for encoding := uint32(1); encoding <= 3; encoding++ {
			_, _, _ = schemaRow(record, encoding)
		}
	})
}
