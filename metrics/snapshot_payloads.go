package metrics

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

const payloadReadBatch = 64

type payloadDestination struct {
	read, block, size int
}

// fetchPayloads fills in the external bodies of every selected block, in
// bounded batches, after the budget has already paid for them.
func fetchPayloads(ctx context.Context, tx sqlite.Reader, reads []seriesRead) error {
	fill, ids, err := newPayloadFill(reads)
	if err != nil {
		return err
	}
	for start := 0; start < len(ids); start += payloadReadBatch {
		if err = fill.fetch(ctx, tx, ids[start:min(start+payloadReadBatch, len(ids))]); err != nil {
			return err
		}
	}
	return nil
}

// payloadFill is where each selected payload goes: the block that takes it,
// and the size that block's directory promised.
type payloadFill struct {
	destinations map[int64]payloadDestination
	reads        []seriesRead
}

func newPayloadFill(reads []seriesRead) (payloadFill, []int64, error) {
	fill := payloadFill{destinations: make(map[int64]payloadDestination), reads: reads}
	ids := make([]int64, 0)
	seen := make(map[int64]bool)
	for readIndex := range reads {
		for blockIndex, block := range reads[readIndex].blocks {
			if block.payload == 0 {
				continue
			}
			if seen[block.payload] {
				return payloadFill{}, nil, fmt.Errorf("%w: selected payload identifier repeats", ErrCorrupt)
			}
			seen[block.payload] = true
			if block.summarized {
				continue
			}
			destination := payloadDestination{read: readIndex, block: blockIndex, size: block.bodyBytes}
			fill.destinations[block.payload] = destination
			ids = append(ids, block.payload)
		}
	}
	return fill, ids, nil
}

const payloadsQuery = `
	select p.id, p.body
	from json_each(?) ids join payloads p on p.id = cast(ids.value as integer)`

func (f payloadFill) fetch(ctx context.Context, tx sqlite.Reader, ids []int64) error {
	encoded, err := json.Marshal(ids)
	if err != nil {
		return fmt.Errorf("encode selected payloads: %w", err)
	}
	rows, err := tx.QueryContext(ctx, payloadsQuery, string(encoded)) //nolint:rowserrcheck // EachRow checks Err
	if err != nil {
		return fmt.Errorf("fetch selected payloads: %w", err)
	}

	fetched := 0
	err = sqlite.EachRow(rows, "selected payloads", func(rows *sql.Rows) error {
		var id int64
		var body []byte
		if scanErr := rows.Scan(&id, &body); scanErr != nil {
			return scanErr
		}
		destination, found := f.destinations[id]
		if !found || len(body) != destination.size {
			return fmt.Errorf("%w: selected payload size or identifier", ErrCorrupt)
		}
		block := &f.reads[destination.read].blocks[destination.block]
		if block.body != nil {
			return fmt.Errorf("%w: selected payload repeats", ErrCorrupt)
		}
		block.body = body
		fetched++
		return nil
	})
	if err != nil {
		return err
	}
	if fetched != len(ids) {
		return fmt.Errorf("%w: selected payload missing", ErrCorrupt)
	}
	return nil
}
