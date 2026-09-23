package main

import (
	"database/sql"
	"fmt"
	"log"
	"time"
)

func benchBounds(baselinePath, candidatePath string) {
	paths := map[string]string{"baseline": baselinePath, "tail_last": candidatePath}
	statements := make(map[string]*sql.Stmt)
	seriesCount := 0
	for name, path := range paths {
		db, err := sql.Open("sqlite", path)
		if err != nil {
			log.Fatal(err)
		}
		defer db.Close()
		db.SetMaxOpenConns(1)
		if _, err := db.Exec(`pragma cache_size=-1024`); err != nil {
			log.Fatal(err)
		}
		var count int
		if err := db.QueryRow(`select count(*) from series_state`).Scan(&count); err != nil {
			log.Fatal(err)
		}
		if count < 1 || seriesCount != 0 && count != seriesCount {
			log.Fatal("candidate has a different series count")
		}
		seriesCount = count
		stmt, err := db.Prepare(`select head_start,head_end from series_state where series_id=?`)
		if err != nil {
			log.Fatal(err)
		}
		defer stmt.Close()
		statements[name] = stmt
	}
	read := func(stmt *sql.Stmt, iterations int) int64 {
		var total int64
		for index := range iterations {
			id := (index*7919)%seriesCount + 1
			var start, end sql.NullInt64
			if err := stmt.QueryRow(id).Scan(&start, &end); err != nil {
				log.Fatal(err)
			}
			total += start.Int64 ^ end.Int64
		}
		return total
	}
	for _, name := range []string{"baseline", "tail_last"} {
		read(statements[name], 10000)
	}
	for _, name := range []string{"baseline", "tail_last", "tail_last", "baseline"} {
		start := time.Now()
		total := read(statements[name], 100000)
		elapsed := time.Since(start)
		fmt.Printf("BOUNDS layout=%s queries=100000 checksum=%d ns_per_query=%.1f\n", name, total, float64(elapsed.Nanoseconds())/100000)
	}
}
