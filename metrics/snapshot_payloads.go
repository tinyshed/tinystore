package metrics

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

const payloadReadBatch = 64

type payloadDestination struct {
	read, block, size int
}

func fetchPayloads(ctx context.Context, tx sqlite.Reader, reads []seriesRead) error {
	addresses := make(map[int64]payloadDestination)
	ids := make([]int64, 0)
	for readIndex := range reads {
		for blockIndex := range reads[readIndex].blocks {
			block := &reads[readIndex].blocks[blockIndex]
			if block.payload == 0 {
				continue
			}
			if _, found := addresses[block.payload]; found {
				return fmt.Errorf("%w: selected payload identifier repeats", ErrCorrupt)
			}
			addresses[block.payload] = payloadDestination{read: readIndex, block: blockIndex, size: block.bodyBytes}
			ids = append(ids, block.payload)
		}
	}
	for start := 0; start < len(ids); start += payloadReadBatch {
		chunk := ids[start:min(start+payloadReadBatch, len(ids))]
		encoded, err := json.Marshal(chunk)
		if err != nil {
			return fmt.Errorf("encode selected payloads: %w", err)
		}
		rows, err := tx.QueryContext(ctx, `select p.id,p.body from json_each(?) ids join payloads p on p.id=cast(ids.value as integer)`, string(encoded))
		if err != nil {
			return fmt.Errorf("fetch selected payloads: %w", err)
		}
		fetched := 0
		for rows.Next() {
			var id int64
			var body []byte
			if err = rows.Scan(&id, &body); err != nil {
				break
			}
			destination, found := addresses[id]
			if !found || len(body) != destination.size {
				err = fmt.Errorf("%w: selected payload size or identifier", ErrCorrupt)
				break
			}
			block := &reads[destination.read].blocks[destination.block]
			if block.body != nil {
				err = fmt.Errorf("%w: selected payload repeats", ErrCorrupt)
				break
			}
			block.body = body
			fetched++
		}
		if err == nil {
			err = rows.Err()
		}
		closeErr := rows.Close() //nolint:sqlclosecheck // release rows before the next bounded batch
		if err != nil {
			return fmt.Errorf("read selected payloads: %w", errors.Join(err, closeErr))
		}
		if closeErr != nil {
			return fmt.Errorf("close selected payloads: %w", closeErr)
		}
		if fetched != len(chunk) {
			return fmt.Errorf("%w: selected payload missing", ErrCorrupt)
		}
	}
	return nil
}
