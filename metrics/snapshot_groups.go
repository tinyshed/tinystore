package metrics

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

const groupReadBatch = 64

type groupAddress struct {
	seriesID, start int64
}

// fetchGroupRows reads the group descriptors of many series in batches,
// charges the budget for each, then fetches only the directories the range
// needs.
func fetchGroupRows(
	ctx context.Context, snapshot snapshotRead, matched []registeredSeries,
) (map[int64][]groupRow, error) {
	groups := make(map[int64][]groupRow, len(matched))
	for first := 0; first < len(matched); first += groupReadBatch {
		end := min(first+groupReadBatch, len(matched))
		batch := groupBatch{groups: groups, sizes: map[groupAddress]int{}, locations: map[groupAddress]int{}}
		if err := batch.readDescriptors(ctx, snapshot, matched[first:end]); err != nil {
			return nil, err
		}
		for offset := 0; offset < len(batch.addresses); offset += groupReadBatch {
			chunk := batch.addresses[offset:min(offset+groupReadBatch, len(batch.addresses))]
			if err := batch.readDirectories(ctx, snapshot.tx, chunk); err != nil {
				return nil, err
			}
		}
	}
	return groups, nil
}

// groupBatch is the groups of up to 64 series: where each group's row sits in
// its series' list, and the directory size its descriptor promised.
type groupBatch struct {
	groups    map[int64][]groupRow
	addresses []groupAddress
	sizes     map[groupAddress]int
	locations map[groupAddress]int
}

// the group that starts at or before from may still hold samples of the range
const groupDescriptorsQuery = `
	select g.series_id, g.start_ts, g.end_ts, length(g.directory), g.clock_id
	from json_each(?) ids join groups g on g.series_id = cast(ids.value as integer)
	where g.start_ts >= coalesce(
	      (select prior.start_ts from groups prior
	       where prior.series_id = g.series_id and prior.start_ts <= ? order by prior.start_ts desc limit 1), ?)
	  and g.start_ts < ? and g.end_ts >= ?
	order by g.series_id, g.start_ts limit cast(? as integer)`

func (b *groupBatch) readDescriptors(ctx context.Context, snapshot snapshotRead, matched []registeredSeries) error {
	ids := make([]int64, len(matched))
	for i, series := range matched {
		ids[i] = series.id
	}
	encoded, err := json.Marshal(ids)
	if err != nil {
		return fmt.Errorf("encode group series: %w", err)
	}
	tx, budget := snapshot.tx, snapshot.budget
	remaining := budget.limits.Blocks - budget.groups + 1
	arguments := []any{string(encoded), snapshot.from, snapshot.from, snapshot.to, snapshot.from, remaining}
	rows, err := tx.QueryContext(ctx, groupDescriptorsQuery, arguments...) //nolint:rowserrcheck // EachRow checks Err
	if err != nil {
		return fmt.Errorf("find metric groups: %w", err)
	}

	return sqlite.EachRow(rows, "group descriptors", func(rows *sql.Rows) error {
		var address groupAddress
		var row groupRow
		var size int
		if scanErr := rows.Scan(&address.seriesID, &address.start, &row.end, &size, &row.clockID); scanErr != nil {
			return scanErr
		}
		row.start = address.start
		return b.take(budget, address, row, size)
	})
}

// take charges one group descriptor to the budget and remembers where its
// directory goes.
func (b *groupBatch) take(budget *queryBudget, address groupAddress, row groupRow, size int) error {
	budget.groups++
	if budget.groups > budget.limits.Blocks {
		return fmt.Errorf("%w: group descriptors", ErrLimit)
	}
	if size < 0 || size > maxDirectoryBytes {
		return fmt.Errorf("%w: group directory size", ErrCorrupt)
	}
	if err := budget.takeBytes(size); err != nil {
		return err
	}
	if _, found := b.sizes[address]; found {
		return fmt.Errorf("%w: repeated group address", ErrCorrupt)
	}
	b.locations[address] = len(b.groups[address.seriesID])
	b.groups[address.seriesID] = append(b.groups[address.seriesID], row)
	b.sizes[address] = size
	b.addresses = append(b.addresses, address)
	return nil
}

const selectedGroupsQuery = `
	select g.series_id, g.start_ts, g.directory
	from json_each(?) selected
	join groups g on g.series_id = cast(json_extract(selected.value, '$[0]') as integer)
	             and g.start_ts = cast(json_extract(selected.value, '$[1]') as integer)`

// readDirectories fetches the selected directories, each exactly as long as
// its descriptor promised.
func (b *groupBatch) readDirectories(ctx context.Context, tx sqlite.Reader, chunk []groupAddress) error {
	pairs := make([][2]int64, len(chunk))
	for i, address := range chunk {
		pairs[i] = [2]int64{address.seriesID, address.start}
	}
	selected, err := json.Marshal(pairs)
	if err != nil {
		return fmt.Errorf("encode selected groups: %w", err)
	}
	rows, err := tx.QueryContext(ctx, selectedGroupsQuery, string(selected)) //nolint:rowserrcheck // EachRow checks Err
	if err != nil {
		return fmt.Errorf("fetch selected groups: %w", err)
	}

	fetched := 0
	err = sqlite.EachRow(rows, "selected groups", func(rows *sql.Rows) error {
		var address groupAddress
		var body []byte
		if scanErr := rows.Scan(&address.seriesID, &address.start, &body); scanErr != nil {
			return scanErr
		}
		position, found := b.locations[address]
		if !found || len(body) != b.sizes[address] || b.groups[address.seriesID][position].data != nil {
			return fmt.Errorf("%w: selected group size or address", ErrCorrupt)
		}
		b.groups[address.seriesID][position].data = body
		fetched++
		return nil
	})
	if err != nil {
		return err
	}
	if fetched != len(chunk) {
		return fmt.Errorf("%w: selected group missing", ErrCorrupt)
	}
	return nil
}
