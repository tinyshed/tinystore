package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
)

type queryBudget struct {
	clocks                         map[int64][]storedBlock
	clockOrder                     []int64
	limits                         Limits
	bytes, decoded, blocks, groups int
}

func (b *queryBudget) takeBytes(size int) error {
	if size < 0 || size > b.limits.PayloadBytes-b.bytes {
		return fmt.Errorf("%w: fetched bytes", ErrLimit)
	}
	b.bytes += size
	return nil
}

func (b *queryBudget) takeSamples(count int) error {
	if count < 0 || count > b.limits.DecodedSamples-b.decoded {
		return fmt.Errorf("%w: decoded samples", ErrLimit)
	}
	b.decoded += count
	return nil
}

type seriesRead struct {
	series registeredSeries
	blocks []storedBlock
	head   headSnapshot
}

// Read returns owned samples, or an error with no partial result; the caller holds no SQLite snapshot.
func (s *Store) Read(ctx context.Context, request Range) ([]Result, error) {
	if err := s.enter(ctx); err != nil {
		return nil, err
	}
	defer s.leave()
	if request.To < request.From {
		return nil, fmt.Errorf("%w: inverted time range", ErrInvalid)
	}
	matchers, _, err := canonicalLabels(request.Matchers, false)
	if err != nil {
		return nil, err
	}
	limits, err := narrowLimits(request.Limits, s.opts.Limits)
	if err != nil {
		return nil, err
	}
	from := max(request.From, s.cutoff())
	if from >= request.To {
		return []Result{}, nil
	}
	reads, err := s.fetchSnapshot(ctx, matchers, from, request.To, limits)
	if err != nil {
		return nil, err
	}
	results, err := s.decodeResults(ctx, reads, from, request.To, limits.OutputSamples)
	if err != nil {
		return nil, err
	}
	s.queried.Add(1)
	return results, nil
}

func (s *Store) fetchSnapshot(ctx context.Context, matchers []Label, from, to int64, limits Limits) ([]seriesRead, error) {
	ctx, cancel := context.WithTimeout(ctx, s.opts.SnapshotTimeout)
	defer cancel()
	var reads []seriesRead
	budget := queryBudget{limits: limits}
	err := s.file.View(ctx, func(tx *sql.Tx) error {
		matched, err := matchSeries(ctx, tx, matchers, &budget)
		if err != nil {
			return err
		}
		for _, series := range matched {
			blocks, readErr := s.fetchBlocks(ctx, tx, series.id, from, to, &budget)
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

func (s *Store) fetchBlocks(ctx context.Context, tx *sql.Tx, id, from, to int64, budget *queryBudget) ([]storedBlock, error) {
	// inspect bounded directories before requesting any external value bytes
	type groupRow struct {
		start, end, clockID int64
		data                []byte
	}
	var rowsToRead []groupRow
	err := func() error {
		rows, err := tx.QueryContext(ctx, `select start_ts,end_ts,length(directory),case when length(directory)<=? then directory else null end,clock_id from groups where series_id=? and start_ts>=coalesce((select start_ts from groups where series_id=? and start_ts<=? order by start_ts desc limit 1),?) and start_ts<? and end_ts>=? order by start_ts limit ?`, budget.limits.PayloadBytes-budget.bytes, id, id, from, from, to, from, budget.limits.Blocks+1)
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
	var blocks []storedBlock
	for _, row := range rowsToRead {
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
				var size int
				if err = tx.QueryRowContext(ctx, `select length(body),case when length(body)=? then body else null end from payloads where id=?`, block.bodyBytes, group.payloadID(slot)).Scan(&size, &block.body); err != nil {
					if errors.Is(err, sql.ErrNoRows) {
						return nil, fmt.Errorf("%w: payload missing", ErrCorrupt)
					}
					return nil, fmt.Errorf("read block payload: %w", err)
				}
				if size != block.bodyBytes || len(block.body) != size {
					return nil, fmt.Errorf("%w: payload size", ErrCorrupt)
				}
			}
			blocks = append(blocks, block)
		}
	}
	return blocks, nil
}

func (s *Store) decodeResults(ctx context.Context, reads []seriesRead, from, to int64, outputLimit int) ([]Result, error) {
	results := make([]Result, 0, len(reads))
	outputCount := 0
	for _, read := range reads {
		result := Result{Series: Series{Labels: read.series.labels, Kind: read.series.kind}}
		appendPoint := func(point Sample) error {
			if point.At < from || point.At >= to {
				return nil
			}
			if outputCount == outputLimit {
				return fmt.Errorf("%w: output samples", ErrLimit)
			}
			if len(result.Samples) > 0 && point.At <= result.Samples[len(result.Samples)-1].At {
				return fmt.Errorf("%w: overlapping samples", ErrCorrupt)
			}
			result.Samples = append(result.Samples, point)
			outputCount++
			return nil
		}
		for _, block := range read.blocks {
			if err := ctx.Err(); err != nil {
				return nil, err
			}
			points, err := s.decodeBlock(block)
			if err != nil {
				return nil, fmt.Errorf("%w: decode values: %w", ErrCorrupt, err)
			}
			for _, point := range points {
				if err = appendPoint(point); err != nil {
					return nil, err
				}
			}
		}
		head, headErr := s.decodeHead(ctx, read.head)
		if headErr != nil {
			return nil, headErr
		}
		for i, point := range head {
			if i%blockSamples == 0 {
				if err := ctx.Err(); err != nil {
					return nil, err
				}
			}
			if err := appendPoint(point); err != nil {
				return nil, err
			}
		}
		if len(result.Samples) > 0 {
			results = append(results, result)
		}
	}
	return results, nil
}
