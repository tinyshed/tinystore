package records

import (
	"cmp"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"maps"
	"math/bits"
	"slices"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// a stream's small segments merge four of a size at a time, so that a record
// is rewritten a few times rather than once a seal: four whose records share
// a power of four make one of the next
//
//	records   30 40 35 38        → one of 143
//	          143 150 160 140    → one of 593
//
// a merged segment keeps its place in the order segments were sealed: its row
// names the segment holding its records and where they start, so a Follow
// cursor that names it still finds each of them once
const (
	mergeFanIn   = 4
	smallRecords = maxSegmentRecords / mergeFanIn // fewer records than this, and
	smallInput   = maxSegmentInput / mergeFanIn   // less input, and four still fit a segment
)

// mergeable is a segment holding its own records, few enough to merge
type mergeable struct {
	id, first, last       int64
	held, input           int
	firstBlock, lastBlock int64
}

// mergeSmall merges the small segments of each stream this pass sealed
func (p *maintenancePass) mergeSmall(ctx context.Context) error {
	for _, stream := range slices.Sorted(maps.Keys(p.sealedStreams)) {
		if err := p.mergeStream(ctx, stream); err != nil {
			return err
		}
	}
	return nil
}

// mergeStream merges until no four of a size are left; a segment that no
// longer reads is reported once, and its stream merges again once it is dropped
func (p *maintenancePass) mergeStream(ctx context.Context, stream int64) error {
	for {
		candidates, err := p.store.mergeable(ctx, stream)
		if err != nil {
			return err
		}
		run := pickMerge(candidates)
		if run == nil {
			return nil
		}
		err = p.store.noted(p.merge(ctx, stream, run))
		var damage *DamageError
		if errors.As(err, &damage) {
			return nil
		}
		if err != nil {
			return err
		}
	}
}

const selectMergeable = `
	select id, first_at, last_at, held, input, first_block, last_block from segments
	where stream = ? and holder is null and held < ? and input < ?
	order by id`

func (s *Store) mergeable(ctx context.Context, stream int64) ([]mergeable, error) {
	var found []mergeable
	err := s.file.ViewPrepared(ctx, func(tx sqlite.Reader) error {
		//nolint:rowserrcheck // EachRow checks Err
		rows, err := tx.QueryContext(ctx, selectMergeable, stream, smallRecords, smallInput)
		if err != nil {
			return err
		}
		return sqlite.EachRow(rows, "mergeable segments", func(rows *sql.Rows) error {
			var m mergeable
			scanErr := rows.Scan(&m.id, &m.first, &m.last, &m.held, &m.input, &m.firstBlock, &m.lastBlock)
			found = append(found, m)
			return scanErr
		})
	})
	if err != nil {
		return nil, fmt.Errorf("records: find segments to merge: %w", err)
	}
	return found, nil
}

// pickMerge chooses the next segments to merge, the smallest size first: four
// of one size, taken in time order and each beginning where the one before
// ended or later, so that their records one after another are in time order
func pickMerge(candidates []mergeable) []mergeable {
	bySize := map[int][]mergeable{}
	for _, candidate := range candidates {
		size := (bits.Len(uint(candidate.held)) + 1) / 2
		bySize[size] = append(bySize[size], candidate)
	}
	for _, size := range slices.Sorted(maps.Keys(bySize)) {
		if run := inTimeOrder(bySize[size]); len(run) == mergeFanIn {
			return run
		}
	}
	return nil
}

// inTimeOrder takes, earliest first, up to four segments that do not overlap;
// one that begins before the last taken ends is left out
func inTimeOrder(segments []mergeable) []mergeable {
	slices.SortFunc(segments, func(a, b mergeable) int {
		return cmp.Or(cmp.Compare(a.first, b.first), cmp.Compare(a.id, b.id))
	})
	var run []mergeable
	for _, segment := range segments {
		if len(run) > 0 && segment.first < run[len(run)-1].last {
			continue
		}
		if run = append(run, segment); len(run) == mergeFanIn {
			break
		}
	}
	return run
}

// merge encodes a run's records as one segment outside the writer, then
// publishes it
func (p *maintenancePass) merge(ctx context.Context, stream int64, run []mergeable) error {
	unreserve, err := p.store.reserve(ctx, func() int64 { return segmentReservation })
	if err != nil {
		return err
	}
	defer unreserve()

	members, err := p.store.readMembers(ctx, run)
	if err != nil {
		return err
	}

	records, err := p.store.memberRecords(stream, members)
	if err != nil {
		return err
	}

	segment := newEncoder(p.store.blobs).encodeSegment(p.store.streams.name(stream), records)
	if err = p.store.publishMerge(ctx, stream, members, segment); err != nil {
		return err
	}

	p.result.MergedSegments += len(run) - 1
	p.result.MergedRecords += segment.count
	p.store.merged.Add(unsigned(len(run) - 1))
	return nil
}

// member is one segment of a merge, and the rows its records are read from
type member struct {
	mergeable
	row    []byte
	blocks [][]byte
}

const (
	selectMemberRow    = `select body from segments where id = ? and holder is null`
	selectMemberBlocks = `select body from blocks where id between ? and ? order by id`
)

// readMembers copies the run's rows out of one snapshot
func (s *Store) readMembers(ctx context.Context, run []mergeable) ([]member, error) {
	members := make([]member, len(run))
	err := s.file.ViewPrepared(ctx, func(tx sqlite.Reader) error {
		for i := range run {
			members[i].mergeable = run[i]
			if err := sqlite.QueryRow(ctx, tx, selectMemberRow, run[i].id).Scan(&members[i].row); err != nil {
				return err
			}
			//nolint:rowserrcheck // EachRow checks Err
			rows, err := tx.QueryContext(ctx, selectMemberBlocks, run[i].firstBlock, run[i].lastBlock)
			if err != nil {
				return err
			}
			err = sqlite.EachRow(rows, "blocks to merge", func(rows *sql.Rows) error {
				var body []byte
				scanErr := rows.Scan(&body)
				members[i].blocks = append(members[i].blocks, body)
				return scanErr
			})
			if err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("records: read segments to merge: %w", err)
	}
	return members, nil
}

// memberRecords decodes each member in the order it stores its records, one
// member after another in time order; a member that no longer reads is its damage
func (s *Store) memberRecords(stream int64, members []member) ([]Record, error) {
	d := newDecoder(s.unpack)
	var records []Record
	for _, m := range members {
		schema, err := d.parseSchema(m.row)
		for i := 0; err == nil && i < len(m.blocks); i++ {
			var decoded []Record
			if decoded, err = d.blockRecords(schema, m.blocks[i]); err == nil {
				records = append(records, decoded...)
			}
		}
		if err != nil {
			found := Damage{Stream: s.streams.name(stream), Segment: m.id, From: timeOf(m.first), To: timeOf(m.last)}
			return nil, damageOf(found, err)
		}
	}
	return records, nil
}

const (
	movePlacesInto = `update segments set holder = ?1, start = start + ?2 where id > ?3 and holder = ?3`
	mergeIntoPlace = `
		update segments set holder = ?, start = start + ?, first_at = null, last_at = null, held = 0, input = 0,
			first_block = 0, last_block = -1, body = x''
		where id = ?`
	rewriteHolder = `
		update segments set first_at = ?, last_at = ?, start = start + ?, held = ?, input = ?,
			first_block = ?, last_block = ?, body = ?
		where id = ?`
)

// publishMerge writes the merged segment into the row of its members' lowest
// id, which keeps its place, deletes the blocks and keys the members were
// made of, and makes every other member a place the first holds, in one
// transaction: a reader finds each record once, before or after it
func (s *Store) publishMerge(ctx context.Context, stream int64, members []member, segment encodedSegment) error {
	holder := slices.MinFunc(members, func(a, b member) int { return cmp.Compare(a.id, b.id) }).id
	begins := beginnings(members)
	err := s.file.UpdatePrepared(ctx, func(tx sqlite.Writer) error {
		if err := deleteMembers(ctx, tx, members); err != nil {
			return err
		}
		firstBlock, err := nextBlockID(ctx, tx)
		if err != nil {
			return err
		}
		place := blockPlace{segment: holder, stream: stream, first: firstBlock}
		if err = insertBlocks(ctx, tx, place, segment.blocks); err != nil {
			return err
		}
		if err = insertKeys(ctx, tx, holder, segment.keys); err != nil {
			return err
		}
		if err = movePlaces(ctx, tx, holder, members, begins); err != nil {
			return err
		}
		lastBlock := firstBlock + int64(len(segment.blocks)) - 1
		_, err = tx.ExecContext(ctx, rewriteHolder, segment.first, segment.last, begins[holder], segment.count,
			segment.input, firstBlock, lastBlock, segment.row, holder)
		return err
	})
	if err != nil {
		return fmt.Errorf("records: publish a merge of %s: %w", segment.stream, err)
	}
	return nil
}

// beginnings is where each member's records begin among the merged segment's
func beginnings(members []member) map[int64]int {
	begins := make(map[int64]int, len(members))
	begin := 0
	for _, m := range members {
		begins[m.id] = begin
		begin += m.held
	}
	return begins
}

func deleteMembers(ctx context.Context, tx sqlite.Writer, members []member) error {
	for _, m := range members {
		for _, statement := range []string{deleteBlockFilters, deleteBlockTraces, deleteBlocks} {
			if _, err := tx.ExecContext(ctx, statement, m.firstBlock, m.lastBlock); err != nil {
				return err
			}
		}
		if _, err := tx.ExecContext(ctx, deleteSegmentKeys, m.id); err != nil {
			return err
		}
	}
	return nil
}

// movePlaces gives the holder the places its members held, then the members
// themselves; the holder's own places move first, so that none moves twice
func movePlaces(ctx context.Context, tx sqlite.Writer, holder int64, members []member, begins map[int64]int) error {
	if _, err := tx.ExecContext(ctx, movePlacesInto, holder, begins[holder], holder); err != nil {
		return err
	}
	for _, m := range members {
		if m.id == holder {
			continue
		}
		if _, err := tx.ExecContext(ctx, movePlacesInto, holder, begins[m.id], m.id); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, mergeIntoPlace, holder, begins[m.id], m.id); err != nil {
			return err
		}
	}
	return nil
}
