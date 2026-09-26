package main

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"io/fs"
	"net/url"
	"os"
	"path/filepath"
	"strings"

	_ "modernc.org/sqlite"
)

const (
	countCells   = `select count(*) from cells`
	objectsQuery = `select name, sum(pgsize), sum(payload), sum(unused) from dbstat group by name order by 2 desc`
	freeQuery    = `select freelist_count * page_size from pragma_freelist_count, pragma_page_size`
)

// reportFile divides the database a closed store left by object through a
// read-only connection: each b-tree's pages, what its cells hold and what they
// leave unused, then the free pages. rows counts the case's rows in sql/app.db,
// expired rows not yet swept included
func reportFile(ctx context.Context, label string, b *backend, rows string) (err error) {
	dirBytes, err := directoryBytes(b.dir)
	if err != nil {
		return err
	}
	info, err := os.Stat(b.file())
	if err != nil {
		return err
	}
	uri, err := readOnly(b.file())
	if err != nil {
		return err
	}
	db, err := sql.Open("sqlite", uri)
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, db.Close()) }()

	if b.impl == implKV {
		rows = countCells
	}
	var count int64
	if err = db.QueryRowContext(ctx, rows).Scan(&count); err != nil {
		return err
	}
	objects, err := fileObjects(ctx, db)
	fmt.Printf("file %s dir_bytes=%d db=%s db_bytes=%d rows=%d bytes_per_row=%.1f\n", label, dirBytes,
		b.fileName(), info.Size(), count, float64(info.Size())/float64(max(count, 1)))
	fmt.Printf("objects %s %s\n", label, objects)
	return err
}

// fileObjects is each b-tree as name=bytes(payload/unused), largest first, and
// the free pages' bytes
func fileObjects(ctx context.Context, db *sql.DB) (string, error) {
	rows, err := db.QueryContext(ctx, objectsQuery)
	if err != nil {
		return "", err
	}
	defer rows.Close()
	var report []string
	for rows.Next() {
		var name string
		var pages, payload, unused int64
		if err = rows.Scan(&name, &pages, &payload, &unused); err != nil {
			return "", err
		}
		report = append(report, fmt.Sprintf("%s=%d(%d/%d)", name, pages, payload, unused))
	}
	if err = rows.Err(); err != nil {
		return "", err
	}
	var free int64
	err = db.QueryRowContext(ctx, freeQuery).Scan(&free)
	return strings.Join(append(report, fmt.Sprintf("free=%d", free)), " "), err
}

// readOnly is a URI that opens path without writing to it
func readOnly(path string) (string, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	uri := &url.URL{Scheme: "file", Path: filepath.ToSlash(abs), RawQuery: "mode=ro"}
	if !strings.HasPrefix(uri.Path, "/") {
		uri.Path = "/" + uri.Path
	}
	return uri.String(), nil
}

func directoryBytes(dir string) (int64, error) {
	var total int64
	err := filepath.WalkDir(dir, func(_ string, entry fs.DirEntry, err error) error {
		if err != nil || entry.IsDir() {
			return err
		}
		info, err := entry.Info()
		if err == nil {
			total += info.Size()
		}
		return err
	})
	return total, err
}
