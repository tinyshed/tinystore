package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

type seriesRead struct {
	series registeredSeries
	blocks []storedBlock
	head   headSnapshot
}

type groupRow struct {
	start, end, clockID int64
	data                []byte
}

func (s *Store) fetchSnapshot(ctx context.Context, matchers []Label, from, to int64, limits Limits) ([]seriesRead, error) {
	ctx, cancel := context.WithTimeout(ctx, s.opts.SnapshotTimeout)
	defer cancel()
	var reads []seriesRead
	budget := queryBudget{limits: limits}
	err := s.file.ViewPrepared(ctx, func(tx sqlite.Reader) error {
		matched, err := matchSeries(ctx, tx, matchers, &budget)
		if err != nil {
			return err
		}
		if len(matched) >= 16 {
			heads, headErr := s.fetchHeads(ctx, tx, matched, from, to, &budget)
			if headErr != nil {
				return headErr
			}
			groups, groupErr := fetchGroupRows(ctx, tx, matched, from, to, &budget)
			if groupErr != nil {
				return groupErr
			}
			for i, series := range matched {
				blocks, readErr := s.decodeGroupRows(ctx, tx, series.id, groups[series.id], from, to, &budget, true)
				if readErr != nil {
					return readErr
				}
				reads = append(reads, seriesRead{series: series, blocks: blocks, head: heads[i]})
			}
			return fetchPayloads(ctx, tx, reads)
		}
		for _, series := range matched {
			blocks, readErr := s.fetchBlocks(ctx, tx, series.id, from, to, &budget, false)
			if readErr != nil {
				return readErr
			}
			head, headErr := s.fetchHead(ctx, tx, series.id, from, to, &budget)
			if headErr != nil {
				return headErr
			}
			reads = append(reads, seriesRead{series: series, blocks: blocks, head: head})
		}
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read metrics snapshot: %w", err)
	}
	return reads, nil
}

func (s *Store) fetchBlocks(ctx context.Context, tx sqlite.Reader, id, from, to int64, budget *queryBudget, deferPayloads bool) ([]storedBlock, error) {
	// inspect bounded directories before requesting any external value bytes
	var rowsToRead []groupRow
	err := func() error {
		rows, err := tx.QueryContext(ctx, `select start_ts,end_ts,length(directory),case when length(directory)<=? then directory else null end,clock_id from groups where series_id=? and start_ts>=coalesce((select start_ts from groups where series_id=? and start_ts<=? order by start_ts desc limit 1),?) and start_ts<? and end_ts>=? order by start_ts limit cast(? as integer)`, budget.limits.PayloadBytes-budget.bytes, id, id, from, from, to, from, budget.limits.Blocks+1)
		if err != nil {
			return fmt.Errorf("find metric groups: %w", err)
		}
		defer rows.Close()
		for rows.Next() {
			var start, end int64
			var size int
			var directory []byte
			var clockID int64
			if err = rows.Scan(&start, &end, &size, &directory, &clockID); err != nil {
				return fmt.Errorf("read group directory: %w", err)
			}
			budget.groups++
			if budget.groups > budget.limits.Blocks {
				return fmt.Errorf("%w: group descriptors", ErrLimit)
			}
			if err = budget.takeBytes(size); err != nil {
				return err
			}
			rowsToRead = append(rowsToRead, groupRow{start: start, end: end, clockID: clockID, data: directory})
		}
		if err = rows.Err(); err != nil {
			return fmt.Errorf("iterate group directories: %w", err)
		}
		return nil
	}()
	if err != nil {
		return nil, err
	}
	return s.decodeGroupRows(ctx, tx, id, rowsToRead, from, to, budget, deferPayloads)
}

func (s *Store) decodeGroupRows(ctx context.Context, tx sqlite.Reader, id int64, rowsToRead []groupRow, from, to int64, budget *queryBudget, deferPayloads bool) ([]storedBlock, error) {
	var blocks []storedBlock
	for _, row := range rowsToRead {
		var err error
		clock, clockErr := loadClock(ctx, tx, row.clockID, budget)
		if clockErr != nil {
			return nil, clockErr
		}
		group, decodeErr := s.readDirectory(id, row.start, row.end, row.clockID, row.data, clock)
		if decodeErr != nil {
			return nil, decodeErr
		}
		for slot, block := range group.blocks {
			if !group.isLive(slot) || block.head.End < from || block.head.Start >= to {
				continue
			}
			budget.blocks++
			if budget.blocks > budget.limits.Blocks {
				return nil, fmt.Errorf("%w: decoded blocks", ErrLimit)
			}
			if err = budget.takeSamples(block.head.Count); err != nil {
				return nil, err
			}
			if group.isExternal(slot) {
				if err = budget.takeBytes(block.bodyBytes); err != nil {
					return nil, err
				}
				block.payload = group.payloadID(slot)
				if !deferPayloads {
					var size int
					if err = sqlite.QueryRow(ctx, tx, `select length(body),case when length(body)=? then body else null end from payloads where id=?`, block.bodyBytes, block.payload).Scan(&size, &block.body); err != nil {
						if errors.Is(err, sql.ErrNoRows) {
							return nil, fmt.Errorf("%w: payload missing", ErrCorrupt)
						}
						return nil, fmt.Errorf("read block payload: %w", err)
					}
					if size != block.bodyBytes || len(block.body) != size {
						return nil, fmt.Errorf("%w: payload size", ErrCorrupt)
					}
				}
			}
			blocks = append(blocks, block)
		}
	}
	return blocks, nil
}
