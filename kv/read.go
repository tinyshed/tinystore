package kv

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"iter"
	"slices"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

const (
	selectLive = `select c.version, c.expires, c.value, c.spill, s.value from cells as c
		left join spilled as s on s.id = c.spill
		where c.bucket = ?1 and c.path = ?2 and (c.expires is null or c.expires > ?3) and not ` + hiddenC
	selectHas = `select version, expires from cells
		where bucket = ?1 and path = ?2 and (expires is null or expires > ?3) and not ` + hiddenCells
	scanBranch = `select c.path, c.version, c.expires, c.value, c.spill, s.value from cells as c
		left join spilled as s on s.id = c.spill
		where c.bucket = ?1 and c.path > ?2 and c.path < ?3 and (c.expires is null or c.expires > ?4)
			and not ` + hiddenC + `
		order by c.path limit cast(?5 as integer)`
)

// Get is the value under key, and whether a live key holds one.
func (b *Bucket[V]) Get(ctx context.Context, key any) (V, bool, error) {
	entry, found, err := b.GetEntry(ctx, key)
	return entry.Value, found, err
}

// GetEntry is Get with the key's version and expiry, the expiry as the file
// has it: a Sliding renewal waiting for its flush is not in it yet.
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

	b.renew(ctx, c, got.version, got.expires)
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
	var version int64
	var expires sql.NullInt64
	err = b.read(ctx, func(r sqlite.Reader) error {
		scanErr := sqlite.QueryRow(ctx, r, selectHas, b.id, c.path, c.now).Scan(&version, &expires)
		if errors.Is(scanErr, sql.ErrNoRows) {
			return nil
		}
		found = scanErr == nil
		return scanErr
	})
	if found {
		b.renew(ctx, c, version, expires)
	}
	return found, b.fail(c, err)
}

// All is every one of this branch's own keys, in Scan's order, a page of Scan
// at a time. Each page is its own snapshot and none is held between pages, so
// a slow loop keeps no reader open, and a key written or deleted during the
// walk may or may not be met; inside View or Tx the pages share its snapshot.
// An error ends the walk as its last element.
func (b *Bucket[V]) All(ctx context.Context) iter.Seq2[Entry[V], error] {
	return func(yield func(Entry[V], error) bool) {
		query := Query{Limit: maxScanLimit}
		for {
			page, err := b.Scan(ctx, query)
			if err != nil {
				yield(Entry[V]{}, err)
				return
			}
			for _, entry := range page.Entries {
				if !yield(entry, nil) {
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

// Scan is a page of this branch's own keys, not those of the branches under
// it, from one snapshot, in the byte order of their text: "10" before "9".
func (b *Bucket[V]) Scan(ctx context.Context, query Query) (Page[V], error) {
	limit, err := b.checkQuery(query)
	if err != nil {
		return Page[V]{}, b.fail(call{key: query.After}, err)
	}
	reserved, err := b.state.reserve(ctx, pageHeld(limit))
	if err != nil {
		return Page[V]{}, err
	}
	defer reserved.Release()

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

// what a row of a page holds beside its path and its value
const rowHeld = 128

// pageHeld is what a page of limit keys may hold while it is read: its values,
// the row past them that says more are left, and every row's path
func pageHeld(limit int) int {
	return min(scanBytes, limit*maxValue) + maxValue + (limit+1)*(maxPath+rowHeld)
}

// scanRows reads at most limit rows, and ends before the row whose value would
// take them past scanBytes; more says that rows were left
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
		if len(rows) == limit {
			more = true
			return errPageFull
		}
		var found row
		if scanErr := result.Scan(&found.path, &found.version, &found.expires, &found.value, &found.spill,
			&found.spilled); scanErr != nil {
			return scanErr
		}
		size := weigh(found.value) + weigh(found.spilled)
		if len(rows) > 0 && bytes+size > scanBytes {
			more = true
			return errPageFull
		}
		bytes += size
		rows = append(rows, found)
		return nil
	})
	if errors.Is(err, errPageFull) {
		err = nil
	}
	return rows, more, err
}

// weigh is what a column of a row holds: its bytes, an integer's eight, or
// nothing where a value spilled or a set keeps none
func weigh(value any) int {
	switch held := value.(type) {
	case nil:
		return 0
	case []byte:
		return len(held)
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
