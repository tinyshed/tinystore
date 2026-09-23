// Command layout measures candidate SQLite layouts on copies of a real corpus file.
package main

import (
	"database/sql"
	"encoding/base64"
	"flag"
	"fmt"
	"log"
	"os"
	"strings"

	_ "modernc.org/sqlite"
)

func main() {
	path := flag.String("db", "", "database copy to inspect")
	other := flag.String("other", "", "comparison database for bounds")
	mode := flag.String("mode", "inspect", "inspect, vacuum, binary_digest, tail_last, bounds or identity")
	flag.Parse()
	if *path == "" {
		log.Fatal("database path required")
	}
	if *mode == "bounds" {
		if *other == "" {
			log.Fatal("comparison database required")
		}
		benchBounds(*path, *other)
		return
	}
	if *mode == "identity" {
		if *other == "" {
			log.Fatal("comparison database required")
		}
		benchIdentity(*path, *other)
		return
	}
	db, err := sql.Open("sqlite", *path)
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()
	db.SetMaxOpenConns(1)
	switch *mode {
	case "inspect":
	case "vacuum":
		if _, err := db.Exec(`vacuum`); err != nil {
			log.Fatal(err)
		}
	case "binary_digest":
		if err := binaryDigest(db); err != nil {
			log.Fatal(err)
		}
	case "tail_last":
		if err := tailLast(db); err != nil {
			log.Fatal(err)
		}
	default:
		log.Fatal("unknown mode")
	}
	rows, err := db.Query(`select name,sql from sqlite_schema where name in ('series','series_state') or (type='index' and tbl_name in ('series','series_state')) order by name`)
	if err != nil {
		log.Fatal(err)
	}
	for rows.Next() {
		var name string
		var definition sql.NullString
		if err := rows.Scan(&name, &definition); err != nil {
			log.Fatal(err)
		}
		fmt.Printf("SCHEMA name=%s sql=%q\n", name, definition.String)
	}
	if err := rows.Err(); err != nil {
		log.Fatal(err)
	}
	rows.Close()
	rows, err = db.Query(`select name,sum(pgsize) from dbstat group by name order by name`)
	if err != nil {
		log.Fatal(err)
	}
	for rows.Next() {
		var name string
		var bytes int64
		if err := rows.Scan(&name, &bytes); err != nil {
			log.Fatal(err)
		}
		fmt.Printf("OBJECT name=%s bytes=%d\n", name, bytes)
	}
	if err := rows.Err(); err != nil {
		log.Fatal(err)
	}
	rows.Close()
	var free, pages, size int64
	if err := db.QueryRow(`select (select freelist_count from pragma_freelist_count),(select page_count from pragma_page_count),(select page_size from pragma_page_size)`).Scan(&free, &pages, &size); err != nil {
		log.Fatal(err)
	}
	info, err := os.Stat(*path)
	if err != nil {
		log.Fatal(err)
	}
	fmt.Printf("FILE bytes=%d pages=%d free_pages=%d page_size=%d\n", info.Size(), pages, free, size)
}

func binaryDigest(db *sql.DB) error {
	rows, err := db.Query(`select id,identity,kind,labels,label_ids from series order by id`)
	if err != nil {
		return err
	}
	type candidate struct {
		id       int64
		digest   []byte
		kind     string
		labels   sql.NullString
		labelIDs []byte
	}
	var series []candidate
	for rows.Next() {
		var entry candidate
		var identity string
		if err := rows.Scan(&entry.id, &identity, &entry.kind, &entry.labels, &entry.labelIDs); err != nil {
			return err
		}
		if !strings.HasPrefix(identity, "@") {
			return fmt.Errorf("unexpected identity format")
		}
		entry.digest, err = base64.RawURLEncoding.DecodeString(identity[1:])
		if err != nil {
			return fmt.Errorf("decode identity digest: %w", err)
		}
		if len(entry.digest) != 32 {
			return fmt.Errorf("identity digest length %d", len(entry.digest))
		}
		series = append(series, entry)
	}
	if err := rows.Err(); err != nil {
		return err
	}
	rows.Close()
	if _, err := db.Exec(`pragma foreign_keys=off`); err != nil {
		return err
	}
	tx, err := db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err := tx.Exec(`create table series_new(id integer primary key, identity blob not null, kind text not null check(kind in ('gauge','counter')), labels text, label_ids blob) strict`); err != nil {
		return err
	}
	stmt, err := tx.Prepare(`insert into series_new(id,identity,kind,labels,label_ids) values(?,?,?,?,?)`)
	if err != nil {
		return err
	}
	for _, entry := range series {
		if _, err := stmt.Exec(entry.id, entry.digest, entry.kind, entry.labels, entry.labelIDs); err != nil {
			return err
		}
	}
	if err := stmt.Close(); err != nil {
		return err
	}
	for _, statement := range []string{`drop table series`, `alter table series_new rename to series`, `create index series_identity on series(identity)`} {
		if _, err := tx.Exec(statement); err != nil {
			return err
		}
	}
	if err := tx.Commit(); err != nil {
		return err
	}
	return finishMigration(db)
}

func tailLast(db *sql.DB) error {
	if _, err := db.Exec(`pragma foreign_keys=off`); err != nil {
		return err
	}
	tx, err := db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	for _, statement := range []string{
		`create table series_state_new(
			series_id integer primary key references series(id),
			max_seen_ts integer not null,
			sealed_before integer,
			version integer not null default 0 check(version >= 0),
			head_count integer not null default 0 check(head_count >= 0),
			ready integer not null default 0 check(ready in (0,1)),
			next_gc_ts integer,
			model_scale integer not null default -2,
			head_start integer,
			head_end integer,
			failed_at integer,
			failure_reason text check(failure_reason is null or length(failure_reason) <= 1024),
			tail blob check(tail is null or length(tail) <= 16777216)) strict`,
		`insert into series_state_new(series_id,max_seen_ts,sealed_before,version,head_count,ready,next_gc_ts,model_scale,head_start,head_end,failed_at,failure_reason,tail)
		 select series_id,max_seen_ts,sealed_before,version,head_count,ready,next_gc_ts,model_scale,head_start,head_end,failed_at,failure_reason,tail from series_state`,
		`drop table series_state`,
		`alter table series_state_new rename to series_state`,
		`create index series_due on series_state(next_gc_ts) where next_gc_ts is not null and failed_at is null`,
		`create index series_failed on series_state(failed_at,series_id) where failed_at is not null`,
		`create index series_failed_id on series_state(series_id) where failed_at is not null`,
		`create index series_ready on series_state(series_id) where ready=1 and failed_at is null`,
	} {
		if _, err := tx.Exec(statement); err != nil {
			return fmt.Errorf("%s: %w", statement, err)
		}
	}
	if err := tx.Commit(); err != nil {
		return err
	}
	return finishMigration(db)
}

func finishMigration(db *sql.DB) error {
	if _, err := db.Exec(`pragma foreign_keys=on`); err != nil {
		return err
	}
	rows, err := db.Query(`pragma foreign_key_check`)
	if err != nil {
		return err
	}
	if rows.Next() {
		rows.Close()
		return fmt.Errorf("foreign keys failed after layout candidate")
	}
	if err := rows.Err(); err != nil {
		return err
	}
	rows.Close()
	var check string
	if err := db.QueryRow(`pragma integrity_check`).Scan(&check); err != nil {
		return err
	}
	if check != "ok" {
		return fmt.Errorf("integrity check: %s", check)
	}
	_, err = db.Exec(`vacuum`)
	return err
}
