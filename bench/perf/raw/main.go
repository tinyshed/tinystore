// Command raw compares prepared database/sql and driver-level reads on one connection.
package main

import (
	"context"
	"database/sql"
	"database/sql/driver"
	"encoding/binary"
	"errors"
	"fmt"
	"hash/crc32"
	"io"
	"log"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"time"

	"modernc.org/libc"
	_ "modernc.org/sqlite"
	sqlite3 "modernc.org/sqlite/lib"
	zsqlite "zombiezen.com/go/sqlite"
)

const query = `select payload from points where id=?`

func prepare(path string) {
	if err := os.Remove(path); err != nil && !os.IsNotExist(err) {
		log.Fatal(err)
	}
	db, err := sql.Open("sqlite", path)
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()
	if _, err := db.Exec(`create table points(id integer primary key, payload blob not null)`); err != nil {
		log.Fatal(err)
	}
	tx, err := db.Begin()
	if err != nil {
		log.Fatal(err)
	}
	stmt, err := tx.Prepare(`insert into points values(?,?)`)
	if err != nil {
		log.Fatal(err)
	}
	defer stmt.Close()
	body := make([]byte, 200)
	for id := 1; id <= 10000; id++ {
		for index := range body {
			body[index] = byte(id + index)
		}
		if _, err := stmt.Exec(id, body); err != nil {
			log.Fatal(err)
		}
	}
	if err := tx.Commit(); err != nil {
		log.Fatal(err)
	}
}

func runSQL(ctx context.Context, conn *sql.Conn, iterations int) (int, error) {
	stmt, err := conn.PrepareContext(ctx, query)
	if err != nil {
		return 0, err
	}
	defer stmt.Close()
	total := 0
	for index := range iterations {
		var payload []byte
		if err := stmt.QueryRowContext(ctx, (index*7919)%10000+1).Scan(&payload); err != nil {
			return 0, err
		}
		total += int(crc32.ChecksumIEEE(payload))
	}
	return total, nil
}

func runRaw(ctx context.Context, conn *sql.Conn, iterations int) (int, error) {
	total := 0
	err := conn.Raw(func(raw any) error {
		preparer, ok := raw.(driver.ConnPrepareContext)
		if !ok {
			return errors.New("driver does not prepare with context")
		}
		stmt, err := preparer.PrepareContext(ctx, query)
		if err != nil {
			return err
		}
		defer stmt.Close()
		query, ok := stmt.(driver.StmtQueryContext)
		if !ok {
			return errors.New("driver statement does not query with context")
		}
		value := make([]driver.Value, 1)
		arguments := []driver.NamedValue{{Ordinal: 1}}
		for index := range iterations {
			arguments[0].Value = int64((index*7919)%10000 + 1)
			rows, err := query.QueryContext(ctx, arguments)
			if err != nil {
				return err
			}
			if err := rows.Next(value); err != nil {
				rows.Close()
				if errors.Is(err, io.EOF) {
					return errors.New("missing point")
				}
				return err
			}
			payload, ok := value[0].([]byte)
			if !ok {
				rows.Close()
				return fmt.Errorf("unexpected payload %T", value[0])
			}
			total += int(crc32.ChecksumIEEE(payload))
			if err := rows.Close(); err != nil {
				return err
			}
		}
		return nil
	})
	return total, err
}

func runProxy(conn *zsqlite.Conn, iterations int) (int, error) {
	stmt := conn.Prep(query)
	buffer := make([]byte, 200)
	total := 0
	for index := range iterations {
		stmt.BindInt64(1, int64((index*7919)%10000+1))
		row, err := stmt.Step()
		if err != nil {
			return 0, err
		}
		if !row {
			return 0, errors.New("missing point")
		}
		size := stmt.ColumnBytes(0, buffer)
		if size != 200 {
			return 0, fmt.Errorf("proxy SQLite payload size %d", size)
		}
		total += int(crc32.ChecksumIEEE(buffer[:size]))
		if err := stmt.Reset(); err != nil {
			return 0, err
		}
	}
	return total, nil
}

func runDirect(conn *sql.Conn, iterations int) (int, error) {
	total := 0
	err := conn.Raw(func(raw any) error {
		connection := reflect.ValueOf(raw).Elem()
		database := connection.FieldByName("db")
		tlsField := connection.FieldByName("tls")
		if !database.IsValid() || !tlsField.IsValid() {
			return fmt.Errorf("modernc connection fields unavailable")
		}
		tls := (*libc.TLS)(tlsField.UnsafePointer())
		queryText, err := libc.CString(query)
		if err != nil {
			return err
		}
		defer libc.Xfree(tls, queryText)
		statementSlot := libc.Xmalloc(tls, 8)
		if statementSlot == 0 {
			return errors.New("allocate SQLite statement slot")
		}
		defer libc.Xfree(tls, statementSlot)
		if result := sqlite3.Xsqlite3_prepare_v2(tls, uintptr(database.Uint()), queryText, -1, statementSlot, 0); result != sqlite3.SQLITE_OK {
			return fmt.Errorf("prepare direct SQLite statement: %d", result)
		}
		statement := uintptr(binary.LittleEndian.Uint64(libc.GoBytes(statementSlot, 8)))
		defer sqlite3.Xsqlite3_finalize(tls, statement)
		for index := range iterations {
			if result := sqlite3.Xsqlite3_bind_int64(tls, statement, 1, int64((index*7919)%10000+1)); result != sqlite3.SQLITE_OK {
				return fmt.Errorf("bind direct SQLite statement: %d", result)
			}
			if result := sqlite3.Xsqlite3_step(tls, statement); result != sqlite3.SQLITE_ROW {
				return fmt.Errorf("step direct SQLite statement: %d", result)
			}
			size := int(sqlite3.Xsqlite3_column_bytes(tls, statement, 0))
			if size != 200 {
				return fmt.Errorf("direct SQLite payload size %d", size)
			}
			payload := sqlite3.Xsqlite3_column_blob(tls, statement, 0)
			if payload == 0 {
				return errors.New("direct SQLite payload missing")
			}
			total += int(crc32.ChecksumIEEE(libc.GoBytes(payload, size)))
			if result := sqlite3.Xsqlite3_reset(tls, statement); result != sqlite3.SQLITE_OK {
				return fmt.Errorf("reset direct SQLite statement: %d", result)
			}
		}
		return nil
	})
	return total, err
}

func main() {
	if len(os.Args) != 2 {
		log.Fatal("usage: raw <directory>")
	}
	path := filepath.Join(os.Args[1], "raw.db")
	if err := os.MkdirAll(os.Args[1], 0o755); err != nil {
		log.Fatal(err)
	}
	prepare(path)
	db, err := sql.Open("sqlite", path)
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()
	db.SetMaxOpenConns(1)
	ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
	defer cancel()
	conn, err := db.Conn(ctx)
	if err != nil {
		log.Fatal(err)
	}
	defer conn.Close()
	if _, err := conn.ExecContext(ctx, `pragma cache_size=-1024`); err != nil {
		log.Fatal(err)
	}
	proxy, err := zsqlite.OpenConn(path, zsqlite.OpenReadOnly)
	if err != nil {
		log.Fatal(err)
	}
	defer proxy.Close()
	proxy.SetInterrupt(ctx.Done())
	proxyCache := proxy.Prep(`pragma cache_size=-1024`)
	if _, err := proxyCache.Step(); err != nil {
		log.Fatal(err)
	}
	if err := proxyCache.Reset(); err != nil {
		log.Fatal(err)
	}
	const iterations = 100000
	expected := 0
	for _, mode := range []string{"sql", "raw", "direct", "proxy", "proxy", "direct", "raw", "sql"} {
		var before, after runtime.MemStats
		runtime.GC()
		runtime.ReadMemStats(&before)
		start := time.Now()
		var total int
		switch mode {
		case "sql":
			total, err = runSQL(ctx, conn, iterations)
		case "raw":
			total, err = runRaw(ctx, conn, iterations)
		case "proxy":
			total, err = runProxy(proxy, iterations)
		case "direct":
			total, err = runDirect(conn, iterations)
		}
		elapsed := time.Since(start)
		runtime.ReadMemStats(&after)
		if expected == 0 {
			expected = total
		}
		if err != nil || total != expected || total == 0 {
			log.Fatalf("%s result %d: %v", mode, total, err)
		}
		fmt.Printf("mode=%s queries=%d ns_per_query=%.1f allocs_per_query=%.2f bytes_alloc_per_query=%.1f\n", mode, iterations, float64(elapsed.Nanoseconds())/iterations, float64(after.Mallocs-before.Mallocs)/iterations, float64(after.TotalAlloc-before.TotalAlloc)/iterations)
	}
}
