// Command blob answers one question the group-size sweep left open: whether
// SQLite's incremental BLOB read lets a grouped payload be read one microblock
// at a time, instead of materialising the whole group in Go.
//
// The driver keeps its sqlite3 handle and its libc TLS in unexported fields,
// so reaching them needs reflection. That is the finding as much as the
// timings are: the C API is translated and callable, the driver just does not
// hand it over.
package main

import (
	"bytes"
	"context"
	"database/sql"
	"flag"
	"fmt"
	"log"
	"math/rand/v2"
	"os"
	"path/filepath"
	"reflect"
	"time"
	"unsafe"

	"modernc.org/libc"
	"modernc.org/libc/sys/types"
	_ "modernc.org/sqlite"
	sqlite3 "modernc.org/sqlite/lib"
)

const (
	microblock = 210 // one ungrouped payload from the NAB run
	pointerLen = int(unsafe.Sizeof(uintptr(0)))
)

type handle struct {
	tls *libc.TLS
	db  uintptr
}

// handles reaches the driver's own sqlite3 handle. Nothing in the public API
// exposes it, so this is exactly the reflection the upstream request would
// remove.
func handles(conn *sql.Conn) (handle, error) {
	var h handle
	err := conn.Raw(func(driverConn any) error {
		value := reflect.ValueOf(driverConn).Elem()
		field := value.FieldByName("db")
		if !field.IsValid() {
			return fmt.Errorf("driver conn has no db field: %T", driverConn)
		}
		h.db = reflect.NewAt(field.Type(), unsafe.Pointer(field.UnsafeAddr())).Elem().Interface().(uintptr)
		field = value.FieldByName("tls")
		if !field.IsValid() {
			return fmt.Errorf("driver conn has no tls field: %T", driverConn)
		}
		h.tls = reflect.NewAt(field.Type(), unsafe.Pointer(field.UnsafeAddr())).Elem().Interface().(*libc.TLS)
		return nil
	})
	return h, err
}

type blobReader struct {
	h              handle
	zDb, zTab, zCl uintptr
	ppBlob, buffer uintptr
	pBlob          uintptr
}

func newBlobReader(h handle, table, column string, size int) (*blobReader, error) {
	r := &blobReader{h: h}
	var err error
	if r.zDb, err = libc.CString("main"); err != nil {
		return nil, err
	}
	if r.zTab, err = libc.CString(table); err != nil {
		return nil, err
	}
	if r.zCl, err = libc.CString(column); err != nil {
		return nil, err
	}
	r.ppBlob = libc.Xmalloc(h.tls, types.Size_t(pointerLen))
	r.buffer = libc.Xmalloc(h.tls, types.Size_t(size))
	if r.ppBlob == 0 || r.buffer == 0 {
		return nil, fmt.Errorf("out of memory")
	}
	return r, nil
}

// read opens the row's blob, or reopens an already open one on the same
// column, and copies just the requested window out
func (r *blobReader) read(rowid int64, offset, length int, into []byte) error {
	if r.pBlob == 0 {
		rc := sqlite3.Xsqlite3_blob_open(r.h.tls, r.h.db, r.zDb, r.zTab, r.zCl, rowid, 0, r.ppBlob)
		if rc != sqlite3.SQLITE_OK {
			return fmt.Errorf("blob_open: %d", rc)
		}
		r.pBlob = *(*uintptr)(unsafe.Pointer(r.ppBlob))
	} else if rc := sqlite3.Xsqlite3_blob_reopen(r.h.tls, r.pBlob, rowid); rc != sqlite3.SQLITE_OK {
		return fmt.Errorf("blob_reopen: %d", rc)
	}
	if rc := sqlite3.Xsqlite3_blob_read(r.h.tls, r.pBlob, r.buffer, int32(length), int32(offset)); rc != sqlite3.SQLITE_OK {
		return fmt.Errorf("blob_read: %d", rc)
	}
	copy(into, unsafe.Slice((*byte)(unsafe.Pointer(r.buffer)), length))
	return nil
}

func (r *blobReader) close() {
	if r.pBlob != 0 {
		sqlite3.Xsqlite3_blob_close(r.h.tls, r.pBlob)
	}
	libc.Xfree(r.h.tls, r.buffer)
	libc.Xfree(r.h.tls, r.ppBlob)
	libc.Xfree(r.h.tls, r.zCl)
	libc.Xfree(r.h.tls, r.zTab)
	libc.Xfree(r.h.tls, r.zDb)
}

func main() {
	total := flag.Int("microblocks", 40000, "microblocks in the file")
	runs := flag.Int("runs", 4000, "point reads per measurement")
	cacheKiB := flag.Int("cache", 2048, "SQLite page cache, KiB")
	flag.Parse()

	fmt.Printf("microblock %d B, %d microblocks, page cache %d KiB, %d reads per measurement\n",
		microblock, *total, *cacheKiB, *runs)
	fmt.Printf("%-7s %9s %9s %11s %11s %11s %11s\n",
		"group", "rows", "file B", "SELECT ns", "ranged ns", "SELECT B", "ranged B", "verified")

	for _, group := range []int{1, 2, 4, 8, 16, 64} {
		measure(group, *total, *runs, *cacheKiB)
	}
}

func measure(group, total, runs, cacheKiB int) {
	ctx := context.Background()
	dir, err := os.MkdirTemp("", "blobprobe")
	if err != nil {
		log.Fatal(err)
	}
	defer os.RemoveAll(dir)
	path := filepath.Join(dir, "probe.db")

	db, err := sql.Open("sqlite", "file:"+path+"?_pragma=busy_timeout(5000)&_txlock=immediate")
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()
	db.SetMaxOpenConns(1)
	db.SetConnMaxLifetime(0)
	for _, statement := range []string{
		"pragma page_size=4096",
		"pragma journal_mode=WAL",
		fmt.Sprintf("pragma cache_size=-%d", cacheKiB),
		"create table payloads (id integer primary key, body blob not null)",
	} {
		if _, err := db.Exec(statement); err != nil {
			log.Fatalf("%s: %v", statement, err)
		}
	}

	rows := total / group
	source := rand.New(rand.NewPCG(1, 2))
	body := make([]byte, group*microblock)
	tx, err := db.Begin()
	if err != nil {
		log.Fatal(err)
	}
	for id := 1; id <= rows; id++ {
		for i := range body {
			body[i] = byte(source.Uint32())
		}
		if _, err := tx.Exec("insert into payloads values(?,?)", id, body); err != nil {
			log.Fatal(err)
		}
	}
	if err := tx.Commit(); err != nil {
		log.Fatal(err)
	}
	if _, err := db.Exec("pragma wal_checkpoint(TRUNCATE)"); err != nil {
		log.Fatal(err)
	}
	info, err := os.Stat(path)
	if err != nil {
		log.Fatal(err)
	}

	// a fresh connection, so neither measurement inherits the writer's warm cache
	if err := db.Close(); err != nil {
		log.Fatal(err)
	}
	db, err = sql.Open("sqlite", "file:"+path+fmt.Sprintf("?_pragma=cache_size(-%d)", cacheKiB))
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()
	db.SetMaxOpenConns(1)
	db.SetConnMaxLifetime(0)

	targets := make([]int64, runs)
	slots := make([]int, runs)
	for i := range targets {
		targets[i] = int64(source.IntN(rows) + 1)
		slots[i] = source.IntN(group)
	}

	scratch := make([]byte, microblock)
	var selectBytes, rangedBytes int64

	// both halves run on this one connection: the pool holds a single
	// connection, so a query through db while conn is checked out would wait
	// for itself, and comparing two different connections' caches would be
	// comparing two different things anyway
	conn, err := db.Conn(ctx)
	if err != nil {
		log.Fatal(err)
	}
	defer conn.Close()

	begin := time.Now()
	for i := range targets {
		var blob []byte
		if err := conn.QueryRowContext(ctx, "select body from payloads where id=?", targets[i]).Scan(&blob); err != nil {
			log.Fatal(err)
		}
		selectBytes += int64(len(blob))
		copy(scratch, blob[slots[i]*microblock:(slots[i]+1)*microblock])
	}
	selectNs := time.Since(begin).Nanoseconds() / int64(runs)

	h, err := handles(conn)
	if err != nil {
		log.Fatalf("reaching the driver handle: %v", err)
	}
	reader, err := newBlobReader(h, "payloads", "body", microblock)
	if err != nil {
		log.Fatal(err)
	}
	defer reader.close()

	// a timing for bytes nobody checked is worthless, so every window the
	// ranged read returns is compared with the same window of the whole blob
	expected := make([]byte, microblock)
	for i := range targets {
		var blob []byte
		if err := conn.QueryRowContext(ctx, "select body from payloads where id=?", targets[i]).Scan(&blob); err != nil {
			log.Fatal(err)
		}
		copy(expected, blob[slots[i]*microblock:(slots[i]+1)*microblock])
		if err := reader.read(targets[i], slots[i]*microblock, microblock, scratch); err != nil {
			log.Fatal(err)
		}
		if !bytes.Equal(scratch, expected) {
			log.Fatalf("ranged read disagrees with the whole blob at row %d slot %d", targets[i], slots[i])
		}
	}

	begin = time.Now()
	for i := range targets {
		if err := reader.read(targets[i], slots[i]*microblock, microblock, scratch); err != nil {
			log.Fatal(err)
		}
		rangedBytes += microblock
	}
	rangedNs := time.Since(begin).Nanoseconds() / int64(runs)

	fmt.Printf("%-7d %9d %9d %11d %11d %11d %11d %11d\n",
		group, rows, info.Size(), selectNs, rangedNs,
		selectBytes/int64(runs), rangedBytes/int64(runs), runs)
}
