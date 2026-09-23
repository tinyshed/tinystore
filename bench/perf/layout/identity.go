package main

import (
	"database/sql"
	"fmt"
	"log"
	"time"
)

func benchIdentity(baselinePath, candidatePath string) {
	type source struct {
		db         *sql.DB
		statement  *sql.Stmt
		identities []any
	}
	sources := map[string]*source{}
	for name, path := range map[string]string{"text": baselinePath, "binary": candidatePath} {
		db, err := sql.Open("sqlite", path)
		if err != nil {
			log.Fatal(err)
		}
		defer db.Close()
		db.SetMaxOpenConns(1)
		if _, err := db.Exec(`pragma cache_size=-1024`); err != nil {
			log.Fatal(err)
		}
		rows, err := db.Query(`select identity from series order by id`)
		if err != nil {
			log.Fatal(err)
		}
		entry := &source{db: db}
		for rows.Next() {
			if name == "text" {
				var identity string
				if err := rows.Scan(&identity); err != nil {
					log.Fatal(err)
				}
				entry.identities = append(entry.identities, identity)
			} else {
				var identity []byte
				if err := rows.Scan(&identity); err != nil {
					log.Fatal(err)
				}
				entry.identities = append(entry.identities, identity)
			}
		}
		if err := rows.Err(); err != nil {
			log.Fatal(err)
		}
		rows.Close()
		entry.statement, err = db.Prepare(`select id,label_ids from series where identity=?`)
		if err != nil {
			log.Fatal(err)
		}
		defer entry.statement.Close()
		sources[name] = entry
	}
	if len(sources["text"].identities) != len(sources["binary"].identities) {
		log.Fatal("different series counts")
	}
	read := func(source *source, iterations int) int64 {
		var total int64
		for index := range iterations {
			id := (index*7919)%len(source.identities) + 1
			var got int64
			var labels []byte
			if err := source.statement.QueryRow(source.identities[id-1]).Scan(&got, &labels); err != nil {
				log.Fatal(err)
			}
			if got != int64(id) || len(labels) == 0 {
				log.Fatal("identity lookup changed")
			}
			total += got
		}
		return total
	}
	for _, name := range []string{"text", "binary"} {
		read(sources[name], 10000)
	}
	for _, name := range []string{"text", "binary", "binary", "text"} {
		start := time.Now()
		total := read(sources[name], 100000)
		elapsed := time.Since(start)
		fmt.Printf("IDENTITY layout=%s queries=100000 checksum=%d ns_per_query=%.1f\n", name, total, float64(elapsed.Nanoseconds())/100000)
	}
}
