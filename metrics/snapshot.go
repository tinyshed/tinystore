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

// snapshotRead is what every fetch inside one read transaction shares.
type snapshotRead struct {
	tx       sqlite.Reader
	from, to int64

	// budget is charged by each fetch.
	budget *queryBudget

	// deferPayloads makes external payloads wait for one batched fetch at the
	// end.
	deferPayloads bool
	aggregate     *aggregateSelection
	// planOnly charges every block to the budget and fetches no payload: a Plan.
	planOnly bool
}

// smaller matches keep one query per series, as tinyshed/research's
// tinystore/reports/batched-head-reads-2026-09-23.md measured
const batchedSeries = 16

// fetchSnapshot copies everything a query needs out of one read transaction,
// which ends before anything is decoded.
func (s *Store) fetchSnapshot(ctx context.Context, query rangeQuery) ([]seriesRead, error) {
	reads, _, err := s.fetchSnapshotSpending(ctx, query)
	return reads, err
}

// fetchSnapshotSpending is fetchSnapshot and what it spent of its budget,
// which a plan reports.
func (s *Store) fetchSnapshotSpending(ctx context.Context, query rangeQuery) ([]seriesRead, queryBudget, error) {
	ctx, cancel := context.WithTimeout(ctx, s.opts.SnapshotTimeout)
	defer cancel()
	var reads []seriesRead
	budget := queryBudget{limits: query.limits}
	err := s.file.ViewPrepared(ctx, func(tx sqlite.Reader) error {
		matched, err := matchSeries(ctx, tx, query.matchers, query.conditions, &budget)
		if err != nil {
			return err
		}
		snapshot := snapshotRead{
			tx: tx, from: query.from, to: query.to, budget: &budget,
			aggregate: query.aggregate, planOnly: query.planOnly,
		}
		if len(matched) >= batchedSeries {
			snapshot.deferPayloads = true
			reads, err = s.fetchBatched(ctx, snapshot, matched)
		} else {
			reads, err = s.fetchEach(ctx, snapshot, matched)
		}
		return err
	})
	if err != nil {
		return nil, budget, fmt.Errorf("read metrics snapshot: %w", err)
	}
	return reads, budget, nil
}

// fetchBatched reads the heads, the group directories and the payloads of
// many series in bounded batches, each paid for before it is fetched.
func (s *Store) fetchBatched(
	ctx context.Context, snapshot snapshotRead, matched []registeredSeries,
) ([]seriesRead, error) {
	heads, err := s.fetchHeads(ctx, snapshot, matched)
	if err != nil {
		return nil, err
	}

	groups, err := fetchGroupRows(ctx, snapshot, matched)
	if err != nil {
		return nil, err
	}

	reads := make([]seriesRead, 0, len(matched))
	for i, series := range matched {
		var blocks []storedBlock
		if blocks, err = s.decodeGroupRows(ctx, snapshot, series.id, groups[series.id]); err != nil {
			return nil, err
		}
		reads = append(reads, seriesRead{series: series, blocks: blocks, head: heads[i]})
	}

	if snapshot.planOnly {
		return reads, nil
	}
	if err = fetchPayloads(ctx, snapshot.tx, reads); err != nil {
		return nil, err
	}
	return reads, nil
}

func (s *Store) fetchEach(
	ctx context.Context, snapshot snapshotRead, matched []registeredSeries,
) ([]seriesRead, error) {
	reads := make([]seriesRead, 0, len(matched))
	for _, series := range matched {
		rows, err := snapshot.groupsInRange(ctx, series.id)
		if err != nil {
			return nil, err
		}
		blocks, err := s.decodeGroupRows(ctx, snapshot, series.id, rows)
		if err != nil {
			return nil, err
		}
		head, err := s.fetchHead(ctx, snapshot.tx, series.id, snapshot.from, snapshot.to, snapshot.budget)
		if err != nil {
			return nil, err
		}
		reads = append(reads, seriesRead{series: series, blocks: blocks, head: head})
	}
	return reads, nil
}

// the group that starts at or before from may still hold samples of the range
const groupsInRangeQuery = `
	select start_ts, end_ts, length(directory),
	       case when length(directory) <= ? then directory else null end, clock_id
	from groups
	where series_id = ?
	  and start_ts >= coalesce(
	      (select start_ts from groups where series_id = ? and start_ts <= ? order by start_ts desc limit 1), ?)
	  and start_ts < ? and end_ts >= ?
	order by start_ts limit cast(? as integer)`

// groupsInRange reads one series' group directories, each charged to the
// budget, before any external value bytes are requested.
func (r snapshotRead) groupsInRange(ctx context.Context, id int64) ([]groupRow, error) {
	budget := r.budget
	arguments := []any{
		budget.limits.PayloadBytes - budget.bytes, id, id, r.from, r.from, r.to, r.from, budget.limits.Blocks + 1,
	}
	rows, err := r.tx.QueryContext(ctx, groupsInRangeQuery, arguments...)
	if err != nil {
		return nil, fmt.Errorf("find metric groups: %w", err)
	}
	defer rows.Close()

	var groups []groupRow
	for rows.Next() {
		var row groupRow
		var size int
		if err = rows.Scan(&row.start, &row.end, &size, &row.data, &row.clockID); err != nil {
			return nil, fmt.Errorf("read group directory: %w", err)
		}
		budget.groups++
		if budget.groups > budget.limits.Blocks {
			return nil, fmt.Errorf("%w: group descriptors", ErrLimit)
		}
		if err = budget.takeBytes(size); err != nil {
			return nil, err
		}
		groups = append(groups, row)
	}
	if err = rows.Err(); err != nil {
		return nil, fmt.Errorf("iterate group directories: %w", err)
	}
	return groups, nil
}

// decodeGroupRows checks each directory against its clock and keeps the live
// blocks that overlap the range.
func (s *Store) decodeGroupRows(
	ctx context.Context, snapshot snapshotRead, id int64, rows []groupRow,
) ([]storedBlock, error) {
	var blocks []storedBlock
	for _, row := range rows {
		clock, err := loadClock(ctx, snapshot.tx, row.clockID, snapshot.budget)
		if err != nil {
			return nil, err
		}
		group, err := s.readDirectory(id, row, clock)
		if err != nil {
			return nil, err
		}
		for slot, block := range group.blocks {
			if !group.isLive(slot) || block.head.End < snapshot.from || block.head.Start >= snapshot.to {
				continue
			}
			if block, err = snapshot.takeBlock(ctx, &group, slot, block); err != nil {
				return nil, err
			}
			blocks = append(blocks, block)
		}
	}
	return blocks, nil
}

// takeBlock charges one block to the budget and, unless the snapshot fetches
// payloads in one batch at the end, reads its external body now.
func (r snapshotRead) takeBlock(
	ctx context.Context, group *blockGroup, slot int, block storedBlock,
) (storedBlock, error) {
	r.budget.blocks++
	if r.budget.blocks > r.budget.limits.Blocks {
		return block, limit(LimitBlocks, r.budget.blocks, r.budget.limits.Blocks)
	}
	if r.aggregate != nil && r.aggregate.complete(block, r.from, r.to) {
		block.summarized = true
		r.budget.summarized++
		return block, nil
	}
	if err := r.budget.takeSamples(block.head.Count); err != nil {
		return block, err
	}
	if !group.isExternal(slot) {
		return block, nil
	}
	if err := r.budget.takeBytes(block.bodyBytes); err != nil {
		return block, err
	}
	if r.deferPayloads || r.planOnly {
		return block, nil
	}
	return block, readPayload(ctx, r.tx, &block)
}

const payloadQuery = `select length(body),case when length(body)=? then body else null end from payloads where id=?`

func readPayload(ctx context.Context, tx sqlite.Reader, block *storedBlock) error {
	var size int
	err := sqlite.QueryRowByKey(ctx, tx, payloadQuery, block.bodyBytes, block.payload).Scan(&size, &block.body)
	if errors.Is(err, sql.ErrNoRows) {
		return fmt.Errorf("%w: payload missing", ErrCorrupt)
	}
	if err != nil {
		return fmt.Errorf("read block payload: %w", err)
	}
	if size != block.bodyBytes || len(block.body) != size {
		return fmt.Errorf("%w: payload size", ErrCorrupt)
	}
	return nil
}
