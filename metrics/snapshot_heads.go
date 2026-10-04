package metrics

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

const headReadBatch = 64

// fetchHeads reads the heads of many series in batches.
//
// It reads descriptors first, so that the budget pays before any packed tail is
// fetched, then only the tails the range needs.
func (s *Store) fetchHeads(
	ctx context.Context, snapshot snapshotRead, matched []registeredSeries,
) ([]headSnapshot, error) {
	heads := make([]headSnapshot, len(matched))
	for start := 0; start < len(matched); start += headReadBatch {
		end := min(start+headReadBatch, len(matched))
		batch := newHeadBatch(matched[start:end], heads[start:end])
		if err := s.readHeadDescriptors(ctx, snapshot, &batch); err != nil {
			return nil, err
		}
		if len(batch.packed) == 0 {
			continue
		}
		if err := batch.readTails(ctx, snapshot.tx); err != nil {
			return nil, err
		}
		if err := s.parseSelectedHeads(snapshot, batch); err != nil {
			return nil, err
		}
	}
	return heads, nil
}

// headBatch is up to 64 heads read together. Its heads are a window of the
// caller's slice, so what the batch fills in, the caller has.
type headBatch struct {
	heads     []headSnapshot
	positions map[int64]int // series id → index in heads
	packed    []int64       // series whose tail the range needs
	sizes     map[int64]int // the tail size each descriptor promised
}

func newHeadBatch(matched []registeredSeries, heads []headSnapshot) headBatch {
	batch := headBatch{
		heads:     heads,
		positions: make(map[int64]int, len(matched)),
		packed:    make([]int64, 0, len(matched)),
		sizes:     make(map[int64]int, len(matched)),
	}
	for i, series := range matched {
		batch.positions[series.id] = i
		heads[i].seriesID = series.id
	}
	return batch
}

// lengths are read first so a selected tail is charged before SQLite fetches its bytes
type headDescriptor struct {
	id          int64
	count, size int
	first, last sql.NullInt64
}

const headDescriptorsQuery = `
	select state.series_id, state.head_count, state.head_start, state.head_end, coalesce(length(state.tail), 0)
	from json_each(?) ids join series_state state on state.series_id = cast(ids.value as integer)`

func (s *Store) readHeadDescriptors(ctx context.Context, snapshot snapshotRead, batch *headBatch) error {
	ids := make([]int64, len(batch.heads))
	for i := range batch.heads {
		ids[i] = batch.heads[i].seriesID
	}
	encoded, err := json.Marshal(ids)
	if err != nil {
		return fmt.Errorf("encode head identifiers: %w", err)
	}
	tx := snapshot.tx
	rows, err := tx.QueryContext(ctx, headDescriptorsQuery, string(encoded)) //nolint:rowserrcheck // EachRow checks Err
	if err != nil {
		return fmt.Errorf("read mutable descriptors: %w", err)
	}

	seen := make(map[int64]bool, len(ids))
	err = sqlite.EachRow(rows, "mutable descriptors", func(rows *sql.Rows) error {
		var row headDescriptor
		if scanErr := rows.Scan(&row.id, &row.count, &row.first, &row.last, &row.size); scanErr != nil {
			return scanErr
		}
		position, found := batch.positions[row.id]
		if !found || seen[row.id] {
			return fmt.Errorf("%w: mutable descriptor identifier", ErrCorrupt)
		}
		seen[row.id] = true
		return s.selectHead(snapshot, batch, position, row)
	})
	if err != nil {
		return err
	}
	if len(seen) != len(ids) {
		return fmt.Errorf("%w: mutable descriptor missing", ErrCorrupt)
	}
	return nil
}

// selectHead checks one descriptor and, when the range needs the head's tail,
// charges its bytes and marks it for fetching.
func (s *Store) selectHead(snapshot snapshotRead, batch *headBatch, position int, row headDescriptor) error {
	head := &batch.heads[position]
	head.count = row.count
	if row.count == 0 {
		if row.first.Valid || row.last.Valid || row.size != 0 {
			return fmt.Errorf("%w: empty mutable head", ErrCorrupt)
		}
		return nil
	}
	if !row.first.Valid || !row.last.Valid || row.last.Int64 < row.first.Int64 {
		return fmt.Errorf("%w: mutable endpoints", ErrCorrupt)
	}
	head.start, head.end = row.first.Int64, row.last.Int64
	if head.end < snapshot.from || head.start >= snapshot.to {
		head.count = 0
		return nil
	}
	if row.count < 0 || row.count > s.opts.MaxHeadSamples || row.size > s.opts.MaxHeadBytes {
		return fmt.Errorf("%w: mutable head capacity", ErrLimit)
	}
	if row.size == 0 {
		return fmt.Errorf("%w: mutable body missing", ErrCorrupt)
	}
	if err := snapshot.budget.takeBytes(row.size); err != nil {
		return err
	}
	batch.packed = append(batch.packed, row.id)
	batch.sizes[row.id] = row.size
	return nil
}

const headTailsQuery = `
	select state.series_id, state.tail
	from json_each(?) ids join series_state state on state.series_id = cast(ids.value as integer)`

// readTails fetches the selected tails, each exactly as long as its
// descriptor promised.
func (b *headBatch) readTails(ctx context.Context, tx sqlite.Reader) error {
	encoded, err := json.Marshal(b.packed)
	if err != nil {
		return fmt.Errorf("encode selected heads: %w", err)
	}
	rows, err := tx.QueryContext(ctx, headTailsQuery, string(encoded)) //nolint:rowserrcheck // EachRow checks Err
	if err != nil {
		return fmt.Errorf("fetch mutable bodies: %w", err)
	}

	fetched := 0
	err = sqlite.EachRow(rows, "mutable bodies", func(rows *sql.Rows) error {
		var id int64
		var body []byte
		if scanErr := rows.Scan(&id, &body); scanErr != nil {
			return scanErr
		}
		position, found := b.positions[id]
		if !found || b.sizes[id] == 0 || b.heads[position].packed != nil || len(body) != b.sizes[id] {
			return fmt.Errorf("%w: mutable body size or identifier", ErrCorrupt)
		}
		b.heads[position].packed = body
		fetched++
		return nil
	})
	if err != nil {
		return err
	}
	if fetched != len(b.packed) {
		return fmt.Errorf("%w: mutable body missing", ErrCorrupt)
	}
	return nil
}

// parseSelectedHeads checks each fetched tail whole, then charges only the
// samples of the chunks the range needs.
func (s *Store) parseSelectedHeads(snapshot snapshotRead, batch headBatch) error {
	for _, id := range batch.packed {
		head := &batch.heads[batch.positions[id]]
		head.filtered, head.from, head.to = true, snapshot.from, snapshot.to
		var err error
		if head.chunks, err = s.parseHead(*head); err != nil {
			return err
		}
		if err = snapshot.chargeHead(head); err != nil {
			return err
		}
	}
	return nil
}

// chargeHead pays for the samples of the head chunks the read decodes: those
// the range touches, or for Latest the newest of them alone.
func (r snapshotRead) chargeHead(head *headSnapshot) error {
	if r.latest {
		head.from = newestChunkFrom(head.chunks, head.from, head.to)
	}
	return r.budget.takeSamples(selectedHeadSamples(head.chunks, head.from, head.to))
}
