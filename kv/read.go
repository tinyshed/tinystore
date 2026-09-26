package kv

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"slices"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

const (
	selectLive = `select c.version, c.expires, c.value, c.spill, s.value from cells as c
		left join spilled as s on s.id = c.spill
		where c.bucket = ?1 and c.path = ?2 and (c.expires is null or c.expires > ?3)`
	selectHas = `select 1 from cells
		where bucket = ?1 and path = ?2 and (expires is null or expires > ?3)`
	scanBranch = `select c.path, c.version, c.expires, c.value, c.spill, s.value from cells as c
		left join spilled as s on s.id = c.spill
		where c.bucket = ?1 and c.path > ?2 and c.path < ?3 and (c.expires is null or c.expires > ?4)
		order by c.path limit cast(?5 as integer)`
)

// Get is the value under key, and whether a live key holds one.
func (b *Bucket[V]) Get(ctx context.Context, key any) (V, bool, error) {
	entry, found, err := b.GetEntry(ctx, key)
	return entry.Value, found, err
}

// GetEntry is Get with the key's version and expiry.
func (b *Bucket[V]) GetEntry(ctx context.Context, key any) (Entry[V], bool, error) {
	c, err := b.begin(key, nil)
	if err != nil {
		return Entry[V]{}, false, b.fail(c, err)
	}

	var got row
	found := false
	err = b.read(ctx, func(r sqlite.Reader) (readErr error) {
		got, found, readErr = readLive(ctx, r, b.id, c)
		return readErr
	})
	if err != nil || !found {
		return Entry[V]{}, false, b.fail(c, err)
	}

	entry, err := b.entryOf(c.key, got)
	return entry, err == nil, b.fail(c, err)
}

// Has says whether a live key is there, without reading its value.
func (b *Bucket[V]) Has(ctx context.Context, key any) (bool, error) {
	c, err := b.begin(key, nil)
	if err != nil {
		return false, b.fail(c, err)
	}

	found := false
	err = b.read(ctx, func(r sqlite.Reader) error {
		var one int
		scanErr := sqlite.QueryRow(ctx, r, selectHas, b.id, c.path, c.now).Scan(&one)
		if errors.Is(scanErr, sql.ErrNoRows) {
			return nil
		}
		found = scanErr == nil
		return scanErr
	})
	return found, b.fail(c, err)
}

// Scan is a page of this branch's own keys, not those of the branches under
// it, from one snapshot, in the byte order of their text: "10" before "9".
func (b *Bucket[V]) Scan(ctx context.Context, query Query) (Page[V], error) {
	limit, err := b.checkQuery(query)
	if err != nil {
		return Page[V]{}, b.fail(call{key: query.After}, err)
	}
	unreserve, err := b.state.reserve(ctx, scanBytes)
	if err != nil {
		return Page[V]{}, err
	}
	defer unreserve()

	var rows []row
	more := false
	from, to, now := b.ownRange(query.After), b.ownEnd(), b.state.now().UnixMilli()
	err = b.read(ctx, func(r sqlite.Reader) (readErr error) {
		rows, more, readErr = scanRows(ctx, r, b.id, [2][]byte{from, to}, now, limit)
		return readErr
	})
	if err != nil {
		return Page[V]{}, b.fail(call{key: query.After}, err)
	}

	return b.page(query, rows, more)
}

func (b *Bucket[V]) checkQuery(query Query) (int, error) {
	switch {
	case b.err != nil:
		return 0, b.err
	case query.Limit < 0 || query.Limit > maxScanLimit:
		return 0, fmt.Errorf("%w: a page of %d keys; at most %d", tinystore.ErrInvalid, query.Limit, maxScanLimit)
	case query.Limit == 0:
		return scanLimit, nil
	}
	return query.Limit, nil
}

// ownRange starts after the branch's key mark, or after the key After
func (b *Bucket[V]) ownRange(after string) []byte {
	if after == "" {
		return append(slices.Clone(b.prefix), keyMark)
	}
	return appendKey(slices.Clone(b.prefix), after)
}

// ownEnd is the first path past the branch's own keys
func (b *Bucket[V]) ownEnd() []byte {
	return append(slices.Clone(b.prefix), keyMark+1)
}

// page turns rows into entries, the next query starting after the last
func (b *Bucket[V]) page(query Query, rows []row, more bool) (Page[V], error) {
	page := Page[V]{Entries: make([]Entry[V], 0, len(rows)), More: more}
	for _, found := range rows {
		entry, err := b.entryOf(keyOf(found.path, len(b.prefix)), found)
		if err != nil {
			return Page[V]{}, b.fail(call{key: entry.Key}, err)
		}
		page.Entries = append(page.Entries, entry)
	}
	if more && len(page.Entries) > 0 {
		page.Next = Query{After: page.Entries[len(page.Entries)-1].Key, Limit: query.Limit}
	}
	return page, nil
}

// row is a key's row as a read returns it; spilled is the value of spilled
// that spill names
type row struct {
	path    []byte
	version int64
	expires sql.NullInt64
	value   any
	spill   sql.NullInt64
	spilled any
}

// held is the value the row keeps, from spilled when it is there
func (r row) held() (any, error) {
	if !r.spill.Valid {
		return r.value, nil
	}
	if r.spilled == nil {
		return nil, fmt.Errorf("%w: the spilled value %d is missing", tinystore.ErrCorrupt, r.spill.Int64)
	}
	return r.spilled, nil
}

func readLive(ctx context.Context, r sqlite.Reader, bucket int64, c call) (row, bool, error) {
	found := row{path: c.path}
	err := sqlite.QueryRow(ctx, r, selectLive, bucket, c.path, c.now).
		Scan(&found.version, &found.expires, &found.value, &found.spill, &found.spilled)
	if errors.Is(err, sql.ErrNoRows) {
		return row{}, false, nil
	}
	return found, err == nil, err
}

// errPageFull ends a scan's rows once a page holds what it may
var errPageFull = errors.New("the page is full")

// scanRows reads at most limit rows, and fewer once their values pass
// scanBytes; more says that rows were left
func scanRows(ctx context.Context, r sqlite.Reader, bucket int64, bounds [2][]byte, now int64, limit int) (
	rows []row, more bool, err error,
) {
	//nolint:rowserrcheck // EachRow checks Err
	result, err := r.QueryContext(ctx, scanBranch, bucket, bounds[0], bounds[1], now, limit+1)
	if err != nil {
		return nil, false, err
	}
	bytes := 0
	err = sqlite.EachRow(result, "a branch", func(result *sql.Rows) error {
		if len(rows) == limit || bytes > scanBytes {
			more = true
			return errPageFull
		}
		var found row
		if scanErr := result.Scan(&found.path, &found.version, &found.expires, &found.value, &found.spill,
			&found.spilled); scanErr != nil {
			return scanErr
		}
		bytes += weigh(found.value) + weigh(found.spilled)
		rows = append(rows, found)
		return nil
	})
	if errors.Is(err, errPageFull) {
		err = nil
	}
	return rows, more, err
}

func weigh(value any) int {
	if raw, ok := value.([]byte); ok {
		return len(raw)
	}
	return 8
}

// entryOf decodes a row's value into an entry of this bucket
func (b *Bucket[V]) entryOf(key string, r row) (Entry[V], error) {
	held, err := r.held()
	if err != nil {
		return Entry[V]{Key: key}, err
	}
	value, err := b.codec.decode(held)
	return Entry[V]{
		Key: key, Value: value, Version: Version{revision: r.version},
		ExpiresAt: expiryTime(r.expires.Int64, r.expires.Valid),
	}, err
}
