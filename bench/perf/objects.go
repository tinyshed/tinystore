package main

import (
	"context"
	"database/sql"
	"log"
	"path/filepath"
)

func objects(ctx context.Context, dir, name string) {
	path := filepath.Join(dir, name)
	db, err := sql.Open("sqlite", "file:"+filepath.ToSlash(path)+"?mode=ro")
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()
	rows, err := db.QueryContext(ctx, `select name,sum(pgsize) from dbstat group by name order by name`)
	if err != nil {
		log.Fatal(err)
	}
	defer rows.Close()
	for rows.Next() {
		var object string
		var size int64
		if err := rows.Scan(&object, &size); err != nil {
			log.Fatal(err)
		}
		report("object", "file", name, "name", object, "bytes", size)
	}
	if err := rows.Err(); err != nil {
		log.Fatal(err)
	}
	var pages, pageSize, free int64
	if err := db.QueryRowContext(ctx, `select (select page_count from pragma_page_count),(select page_size from pragma_page_size),(select freelist_count from pragma_freelist_count)`).Scan(&pages, &pageSize, &free); err != nil {
		log.Fatal(err)
	}
	report("pages", "file", name, "pages", pages, "page_bytes", pageSize, "free_pages", free)
}
