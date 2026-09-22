package metrics

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

const headReadBatch = 64

func (s *Store) fetchHeads(ctx context.Context, tx sqlite.Reader, matched []registeredSeries, from, to int64, budget *queryBudget) ([]headSnapshot, error) {
	heads := make([]headSnapshot, len(matched))
	for start := 0; start < len(matched); start += headReadBatch {
		end := min(start+headReadBatch, len(matched))
		ids := make([]int64, end-start)
		positions := make(map[int64]int, len(ids))
		for i, series := range matched[start:end] {
			ids[i] = series.id
			positions[series.id] = start + i
			heads[start+i].seriesID = series.id
		}
		encoded, err := json.Marshal(ids)
		if err != nil {
			return nil, fmt.Errorf("encode head identifiers: %w", err)
		}
		rows, err := tx.QueryContext(ctx, `select state.series_id,state.head_count,state.head_start,state.head_end,coalesce(length(state.tail),0) from json_each(?) ids join series_state state on state.series_id=cast(ids.value as integer)`, string(encoded))
		if err != nil {
			return nil, fmt.Errorf("read mutable descriptors: %w", err)
		}
		seen := make(map[int64]bool, len(ids))
		packedIDs := make([]int64, 0, len(ids))
		legacyIDs := make([]int64, 0)
		sizes := make(map[int64]int, len(ids))
		for rows.Next() {
			var id int64
			var count, size int
			var first, last sql.NullInt64
			if err = rows.Scan(&id, &count, &first, &last, &size); err != nil {
				break
			}
			position, found := positions[id]
			if !found || seen[id] {
				err = fmt.Errorf("%w: mutable descriptor identifier", ErrCorrupt)
				break
			}
			seen[id] = true
			head := &heads[position]
			head.count = count
			if count == 0 {
				if first.Valid || last.Valid || size != 0 {
					err = fmt.Errorf("%w: empty mutable head", ErrCorrupt)
					break
				}
				continue
			}
			if !first.Valid || !last.Valid || last.Int64 < first.Int64 {
				err = fmt.Errorf("%w: mutable endpoints", ErrCorrupt)
				break
			}
			head.start, head.end = first.Int64, last.Int64
			if head.end < from || head.start >= to {
				head.count = 0
				continue
			}
			if count < 0 || count > s.opts.MaxHeadSamples || size > s.opts.MaxHeadBytes {
				err = fmt.Errorf("%w: mutable head capacity", ErrLimit)
				break
			}
			if size == 0 {
				legacyIDs = append(legacyIDs, id)
				continue
			}
			if err = budget.takeSamples(count); err != nil {
				break
			}
			if err = budget.takeBytes(size); err != nil {
				break
			}
			packedIDs = append(packedIDs, id)
			sizes[id] = size
		}
		if err == nil {
			err = rows.Err()
		}
		closeErr := rows.Close() //nolint:sqlclosecheck // release rows before fetching bodies on the same connection
		if err != nil {
			return nil, fmt.Errorf("read mutable descriptors: %w", errors.Join(err, closeErr))
		}
		if closeErr != nil {
			return nil, fmt.Errorf("close mutable descriptors: %w", closeErr)
		}
		if len(seen) != len(ids) {
			return nil, fmt.Errorf("%w: mutable descriptor missing", ErrCorrupt)
		}
		for _, id := range legacyIDs {
			head, fetchErr := s.fetchHead(ctx, tx, id, from, to, budget)
			if fetchErr != nil {
				return nil, fetchErr
			}
			heads[positions[id]] = head
		}
		if len(packedIDs) == 0 {
			continue
		}
		packedJSON, err := json.Marshal(packedIDs)
		if err != nil {
			return nil, fmt.Errorf("encode selected heads: %w", err)
		}
		bodies, err := tx.QueryContext(ctx, `select state.series_id,state.tail from json_each(?) ids join series_state state on state.series_id=cast(ids.value as integer)`, string(packedJSON))
		if err != nil {
			return nil, fmt.Errorf("fetch mutable bodies: %w", err)
		}
		fetched := 0
		for bodies.Next() {
			var id int64
			var body []byte
			if err = bodies.Scan(&id, &body); err != nil {
				break
			}
			position, found := positions[id]
			if !found || sizes[id] == 0 || heads[position].packed != nil || len(body) != sizes[id] {
				err = fmt.Errorf("%w: mutable body size or identifier", ErrCorrupt)
				break
			}
			heads[position].packed = body
			fetched++
		}
		if err == nil {
			err = bodies.Err()
		}
		closeErr = bodies.Close() //nolint:sqlclosecheck // release bodies before the next batch on the same connection
		if err != nil {
			return nil, fmt.Errorf("read mutable bodies: %w", errors.Join(err, closeErr))
		}
		if closeErr != nil {
			return nil, fmt.Errorf("close mutable bodies: %w", closeErr)
		}
		if fetched != len(packedIDs) {
			return nil, fmt.Errorf("%w: mutable body missing", ErrCorrupt)
		}
	}
	return heads, nil
}
