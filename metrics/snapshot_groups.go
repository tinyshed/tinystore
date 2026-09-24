package metrics

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

const groupReadBatch = 64

type groupAddress struct {
	seriesID, start int64
}

func fetchGroupRows(ctx context.Context, tx sqlite.Reader, matched []registeredSeries, from, to int64, budget *queryBudget) (map[int64][]groupRow, error) {
	groups := make(map[int64][]groupRow, len(matched))
	for first := 0; first < len(matched); first += groupReadBatch {
		end := min(first+groupReadBatch, len(matched))
		ids := make([]int64, end-first)
		for i, series := range matched[first:end] {
			ids[i] = series.id
		}
		encoded, err := json.Marshal(ids)
		if err != nil {
			return nil, fmt.Errorf("encode group series: %w", err)
		}
		remaining := budget.limits.Blocks - budget.groups + 1
		rows, err := tx.QueryContext(ctx, `select g.series_id,g.start_ts,g.end_ts,length(g.directory),g.clock_id from json_each(?) ids join groups g on g.series_id=cast(ids.value as integer) where g.start_ts>=coalesce((select prior.start_ts from groups prior where prior.series_id=g.series_id and prior.start_ts<=? order by prior.start_ts desc limit 1),?) and g.start_ts<? and g.end_ts>=? order by g.series_id,g.start_ts limit cast(? as integer)`, string(encoded), from, from, to, from, remaining)
		if err != nil {
			return nil, fmt.Errorf("find metric groups: %w", err)
		}
		addresses := make([]groupAddress, 0)
		sizes := make(map[groupAddress]int)
		locations := make(map[groupAddress]int)
		for rows.Next() {
			var id, start, endTS, clockID int64
			var size int
			if err = rows.Scan(&id, &start, &endTS, &size, &clockID); err != nil {
				break
			}
			budget.groups++
			if budget.groups > budget.limits.Blocks {
				err = fmt.Errorf("%w: group descriptors", ErrLimit)
				break
			}
			if size < 0 || size > maxDirectoryBytes {
				err = fmt.Errorf("%w: group directory size", ErrCorrupt)
				break
			}
			if err = budget.takeBytes(size); err != nil {
				break
			}
			address := groupAddress{seriesID: id, start: start}
			if _, found := sizes[address]; found {
				err = fmt.Errorf("%w: repeated group address", ErrCorrupt)
				break
			}
			locations[address] = len(groups[id])
			groups[id] = append(groups[id], groupRow{start: start, end: endTS, clockID: clockID})
			sizes[address] = size
			addresses = append(addresses, address)
		}
		if err == nil {
			err = rows.Err()
		}
		closeErr := rows.Close() //nolint:sqlclosecheck // release descriptor rows before fetching directories
		if err != nil {
			return nil, fmt.Errorf("read group descriptors: %w", errors.Join(err, closeErr))
		}
		if closeErr != nil {
			return nil, fmt.Errorf("close group descriptors: %w", closeErr)
		}
		for offset := 0; offset < len(addresses); offset += groupReadBatch {
			chunk := addresses[offset:min(offset+groupReadBatch, len(addresses))]
			pairs := make([][2]int64, len(chunk))
			for i, address := range chunk {
				pairs[i] = [2]int64{address.seriesID, address.start}
			}
			selected, encodeErr := json.Marshal(pairs)
			if encodeErr != nil {
				return nil, fmt.Errorf("encode selected groups: %w", encodeErr)
			}
			bodies, queryErr := tx.QueryContext(ctx, `select g.series_id,g.start_ts,g.directory from json_each(?) selected join groups g on g.series_id=cast(json_extract(selected.value,'$[0]') as integer) and g.start_ts=cast(json_extract(selected.value,'$[1]') as integer)`, string(selected))
			if queryErr != nil {
				return nil, fmt.Errorf("fetch selected groups: %w", queryErr)
			}
			fetched := 0
			for bodies.Next() {
				var id, start int64
				var body []byte
				if err = bodies.Scan(&id, &start, &body); err != nil {
					break
				}
				address := groupAddress{seriesID: id, start: start}
				position, found := locations[address]
				if !found || len(body) != sizes[address] || groups[id][position].data != nil {
					err = fmt.Errorf("%w: selected group size or address", ErrCorrupt)
					break
				}
				groups[id][position].data = body
				fetched++
			}
			if err == nil {
				err = bodies.Err()
			}
			closeErr = bodies.Close() //nolint:sqlclosecheck // release bodies before the next bounded batch
			if err != nil {
				return nil, fmt.Errorf("read selected groups: %w", errors.Join(err, closeErr))
			}
			if closeErr != nil {
				return nil, fmt.Errorf("close selected groups: %w", closeErr)
			}
			if fetched != len(chunk) {
				return nil, fmt.Errorf("%w: selected group missing", ErrCorrupt)
			}
		}
	}
	return groups, nil
}
