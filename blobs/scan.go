package blobs

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"iter"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

var (
	scanFolder = `select o.path, o.size, o.etag, o.type, o.modified, o.expires, o.meta from objects as o
		where o.bucket = ?1 and o.path >= ?2 and o.path < ?3 and (o.expires is null or o.expires > ?4)
			and not ` + hidden + `
		order by o.path limit cast(?5 as integer)`
	countFolder = `select count(*), coalesce(sum(o.size), 0) from objects as o
		where o.bucket = ?1 and o.path >= ?2 and o.path < ?3 and (o.expires is null or o.expires > ?4)
			and not ` + hidden
)

// Scan is a page of every key under this handle's folder, the keys of the
// folders under it included, from one snapshot, in the byte order of their
// paths: 10.jpg before 9.jpg. With Query.Prefix it is the keys that start
// with it. A page holds at most 1000 objects.
func (b *Bucket) Scan(ctx context.Context, query Query) (Page, error) {
	limit, from, to, err := b.checkQuery(query)
	if err != nil {
		return Page{}, b.fail(call{path: b.folder + query.Prefix}, err)
	}
	leave, err := b.store.admit(ctx)
	if err != nil {
		return Page{}, err
	}
	defer leave()
	reserved, err := b.store.reserve(ctx, (limit+1)*objectHeld)
	if err != nil {
		return Page{}, err
	}
	defer reserved.Release()

	rows, more, err := b.scanRows(ctx, from, to, limit)
	if err != nil {
		return Page{}, b.fail(call{path: from}, err)
	}
	return b.page(query, rows, more)
}

// checkQuery is a query's limit and the range of paths it reads: from the
// folder and its prefix, or past After, to the first path past the prefix
//
//	folder users/42/, Prefix "sent/", After "sent/7" → ["users/42/sent/7\x00", "users/42/sent0")
func (b *Bucket) checkQuery(query Query) (limit int, from, to string, err error) {
	switch {
	case b.err != nil:
		return 0, "", "", b.err
	case query.Limit < 0 || query.Limit > maxScanLimit:
		return 0, "", "", fmt.Errorf("%w: a page of %d objects; at most %d", tinystore.ErrInvalid, query.Limit,
			maxScanLimit)
	}
	if err = errors.Join(checkPrefix(query.Prefix), checkPrefix(query.After)); err != nil {
		return 0, "", "", err
	}
	limit = query.Limit
	if limit == 0 {
		limit = scanLimit
	}
	from = b.folder + query.Prefix
	if query.After != "" {
		from = max(from, b.folder+query.After+"\x00")
	}
	return limit, from, prefixEnd(b.folder + query.Prefix), nil
}

// scanRows reads at most limit rows of the range, asking for one more to learn
// whether more are left
func (b *Bucket) scanRows(ctx context.Context, from, to string, limit int) (rows []objectRow, more bool, err error) {
	now := b.store.clock()
	err = b.store.file.Lookup(ctx, func(r sqlite.Reader) error {
		//nolint:rowserrcheck // EachRow checks Err
		result, queryErr := r.QueryContext(ctx, scanFolder, b.id, from, to, now, limit+1)
		if queryErr != nil {
			return queryErr
		}
		return sqlite.EachRow(result, "a folder", func(result *sql.Rows) error {
			var found objectRow
			if scanErr := result.Scan(found.fields()...); scanErr != nil {
				return scanErr
			}
			rows = append(rows, found)
			return nil
		})
	})
	if len(rows) > limit {
		rows, more = rows[:limit], true
	}
	return rows, more, err
}

// page turns rows into objects, the next query starting after the last
func (b *Bucket) page(query Query, rows []objectRow, more bool) (Page, error) {
	page := Page{Objects: make([]Object, 0, len(rows)), More: more}
	for _, found := range rows {
		object, err := found.object(b.folder)
		if err != nil {
			return Page{}, b.fail(call{path: found.path}, err)
		}
		page.Objects = append(page.Objects, object)
	}
	if more && len(page.Objects) > 0 {
		page.Next = Query{Prefix: query.Prefix, After: page.Objects[len(page.Objects)-1].Key, Limit: query.Limit}
	}
	return page, nil
}

// All is every key Scan finds, a page of Scan at a time. Each page is its own
// snapshot and none is held between pages, so a slow loop keeps no reader
// open, and an object written or deleted during the walk may or may not be
// met. An error ends the walk as its last element.
func (b *Bucket) All(ctx context.Context, query Query) iter.Seq2[Object, error] {
	return func(yield func(Object, error) bool) {
		if query.Limit == 0 {
			query.Limit = maxScanLimit
		}
		for {
			page, err := b.Scan(ctx, query)
			if err != nil {
				yield(Object{}, err)
				return
			}
			for _, object := range page.Objects {
				if !yield(object, nil) {
					return
				}
			}
			if !page.More {
				return
			}
			query = page.Next
		}
	}
}

// Usage is the objects under this handle's folder and their bytes, the
// folders under it included, counted from their rows in one snapshot: about
// a millisecond over ten thousand objects, 90 over a million. An expired
// object and one a Clear hid are not counted. It is a check and not a limit:
// two uploads that each pass it can pass a quota together.
func (b *Bucket) Usage(ctx context.Context) (Usage, error) {
	if b.err != nil {
		return Usage{}, b.err
	}
	leave, err := b.store.admit(ctx)
	if err != nil {
		return Usage{}, err
	}
	defer leave()

	var usage Usage
	now := b.store.clock()
	err = b.store.file.Lookup(ctx, func(r sqlite.Reader) error {
		return sqlite.QueryRow(ctx, r, countFolder, b.id, b.folder, prefixEnd(b.folder), now).
			Scan(&usage.Objects, &usage.Bytes)
	})
	return usage, b.fail(call{path: b.folder}, err)
}
