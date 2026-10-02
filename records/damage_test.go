package records

import (
	"bytes"
	"database/sql"
	"errors"
	"log/slog"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// damage changes one byte of a stored body, which its checksum then refuses
func (s *testStore) damage(t *testing.T, table string, id int64) {
	t.Helper()
	statement := `update ` + table + ` set body = cast(substr(body, 1, 20)
		|| (case when substr(body, 21, 1) = x'ff' then x'00' else x'ff' end) || substr(body, 22) as blob)
		where id = ?`
	err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, execErr := tx.ExecContext(t.Context(), statement, id)
		return execErr
	})
	if err != nil {
		t.Fatal(err)
	}
}

// ids lists a table's ids in order, as a test picks one to damage
func (s *testStore) ids(t *testing.T, query string, arguments ...any) []int64 {
	t.Helper()
	var ids []int64
	err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		rows, err := tx.QueryContext(t.Context(), query, arguments...)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var id int64
			if err = rows.Scan(&id); err != nil {
				return err
			}
			ids = append(ids, id)
		}
		return rows.Err()
	})
	if err != nil {
		t.Fatal(err)
	}
	return ids
}

// A head row that no longer reads is logged once and left, and the rest of its
// head seals around it. A read over it names it, and Drop removes it so that
// reads work again.
func TestADamagedHeadRowIsReportedOnceAndTheRestOfItsHeadSeals(t *testing.T) {
	var logged bytes.Buffer
	runtime := tinystore.Options{Logger: slog.New(slog.NewTextHandler(&logged, nil))}
	s := openTestStore(t, t.TempDir(), Options{}, runtime)
	records := frontendRecords(300)
	for start := 0; start < len(records); start += 100 {
		s.append(t, records[start:start+100]...)
	}
	damaged := s.ids(t, `select id from heads order by id`)[1]
	s.damage(t, "heads", damaged)

	s.clock.advance(time.Hour)
	if work := s.maintain(t); work != (Maintenance{SealedSegments: 1, SealedRecords: 200, Damaged: 1}) {
		t.Fatalf("the first pass: %+v", work)
	}
	if work := s.maintain(t); work != (Maintenance{}) {
		t.Fatalf("the second pass: %+v", work)
	}
	if lines := strings.Count(logged.String(), "a row no longer reads"); lines != 1 {
		t.Fatalf("the damage logged %d times, want once", lines)
	}
	if list := s.Damaged(); len(list) != 1 || list[0].HeadRow != damaged || list[0].Stream != "frontend" ||
		list[0].Reason == "" || s.Stats().Damaged != 1 {
		t.Fatalf("damaged %+v, stats %+v", list, s.Stats())
	}

	_, err := s.Scan(t.Context(), Query{})
	var met *DamageError
	if !errors.As(err, &met) || met.Damage.HeadRow != damaged || !errors.Is(err, tinystore.ErrCorrupt) {
		t.Fatalf("a read over the damaged row: %v", err)
	}
	if err = s.Drop(t.Context(), met.Damage); err != nil {
		t.Fatal(err)
	}
	sameRecords(t, sortedByTime(slices.Concat(records[:100], records[200:])), s.readAll(t, Query{}))
	s.headStateMatchesItsRows(t)
	if len(s.Damaged()) != 0 || s.Stats().Damaged != 0 {
		t.Fatalf("a dropped row is still listed: %+v", s.Damaged())
	}
}

// a segment with a block that no longer reads stops a follower until Drop
// removes it whole; the follower then counts it as removed and goes on
func TestDropRemovesADamagedSegmentAndFollowPassesIt(t *testing.T) {
	s := openRecords(t)
	lost, kept := frontendRecords(maxSegmentRecords), backendRecords(1000)
	s.append(t, lost...)
	s.maintain(t)
	s.append(t, kept...)
	s.clock.advance(time.Hour)
	s.maintain(t)
	segment := s.ids(t, `select id from segments order by id`)[0]
	s.damage(t, "blocks", s.ids(t, `select id from blocks where segment = ? order by id`, segment)[0])

	_, err := s.Follow(t.Context(), Cursor{}, 100)
	var met *DamageError
	if !errors.As(err, &met) || met.Damage.Segment != segment || met.Damage.Stream != "frontend" {
		t.Fatalf("a follow over the damaged segment: %v", err)
	}
	if err = s.Drop(t.Context(), met.Damage); err != nil {
		t.Fatal(err)
	}

	batch, err := s.Follow(t.Context(), Cursor{Segment: segment}, 2000)
	if err != nil || batch.Expired != 1 {
		t.Fatalf("following past the dropped segment: expired %d, %v", batch.Expired, err)
	}
	sameRecords(t, kept, batch.Records)
	if got := s.readAll(t, Query{}); len(got) != len(kept) {
		t.Fatalf("read %d records after the drop, want %d", len(got), len(kept))
	}
	if c := s.counts(t); c.segments != 1 || c.blocks != 1 || c.heads != 0 {
		t.Fatalf("tables after the drop: %+v", c)
	}
}

func TestAMissingSegmentBlockIsDamageFollowCanPassAfterDrop(t *testing.T) {
	for _, test := range []struct {
		name, at string
		count    int
	}{
		{"first", "first", 3000},
		{"middle", "middle", 3000},
		{"only", "only", 1},
	} {
		t.Run(test.name, func(t *testing.T) {
			s := openRecords(t)
			s.append(t, frontendRecords(test.count)...)
			s.clock.advance(time.Hour)
			s.maintain(t)
			segment := s.ids(t, `select id from segments order by id`)[0]
			blocks := s.ids(t, `select id from blocks where segment = ? order by id`, segment)
			at := 0
			if test.at == "middle" {
				at = len(blocks) / 2
			}
			if test.at == "only" && len(blocks) != 1 || test.at == "middle" && len(blocks) < 3 {
				t.Fatalf("test has %d blocks", len(blocks))
			}
			err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
				_, deleteErr := tx.ExecContext(t.Context(), `delete from blocks where id = ?`, blocks[at])
				return deleteErr
			})
			if err != nil {
				t.Fatal(err)
			}
			_, err = s.Follow(t.Context(), Cursor{}, 100)
			broken, ok := errors.AsType[*DamageError](err)
			if !ok || broken.Damage.Segment != segment {
				t.Fatalf("missing block: %v", err)
			}
			if err = s.Drop(t.Context(), broken.Damage); err != nil {
				t.Fatalf("Drop missing block: %v", err)
			}
			batch, err := s.Follow(t.Context(), Cursor{Segment: segment}, 100)
			if err != nil || batch.Expired != 1 {
				t.Fatalf("after Drop: %+v, %v", batch, err)
			}
		})
	}
}

func TestChangedIndexCountsAndSizesAreDamage(t *testing.T) {
	t.Run("head", func(t *testing.T) {
		s := openRecords(t)
		s.append(t, frontendRecords(2)...)
		id := s.ids(t, `select id from heads`)[0]
		err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
			_, updateErr := tx.ExecContext(t.Context(), `update heads set count = count + 1, size = -1 where id = ?`, id)
			return updateErr
		})
		if err != nil {
			t.Fatal(err)
		}
		_, err = s.Scan(t.Context(), Query{})
		broken, ok := errors.AsType[*DamageError](err)
		if !ok || broken.Damage.HeadRow != id {
			t.Fatalf("changed head index: %v", err)
		}
		if err = s.Drop(t.Context(), broken.Damage); err != nil {
			t.Fatal(err)
		}
		s.headStateMatchesItsRows(t)
	})
	t.Run("block", func(t *testing.T) {
		s := openRecords(t)
		s.append(t, frontendRecords(1000)...)
		s.clock.advance(time.Hour)
		s.maintain(t)
		id := s.ids(t, `select id from blocks`)[0]
		err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
			_, updateErr := tx.ExecContext(t.Context(), `update blocks set count = count - 1, size = -1 where id = ?`, id)
			return updateErr
		})
		if err != nil {
			t.Fatal(err)
		}
		_, err = s.Follow(t.Context(), Cursor{}, 100)
		broken, ok := errors.AsType[*DamageError](err)
		if !ok || broken.Damage.Segment == 0 {
			t.Fatalf("changed block index: %v", err)
		}
		if err = s.Drop(t.Context(), broken.Damage); err != nil {
			t.Fatal(err)
		}
	})
}

// Drop removes only what is lost already: a row that reads is refused
func TestDropRefusesWhatStillReads(t *testing.T) {
	s := openRecords(t)
	s.append(t, frontendRecords(maxSegmentRecords)...)
	s.maintain(t)
	s.append(t, backendRecords(10)...)
	healthy := map[string]Damage{
		"a segment that reads":  {Segment: s.ids(t, `select id from segments`)[0]},
		"a head row that reads": {HeadRow: s.ids(t, `select id from heads`)[0]},
	}
	for name, damage := range healthy {
		if err := s.Drop(t.Context(), damage); !errors.Is(err, tinystore.ErrConflict) {
			t.Errorf("%s: %v", name, err)
		}
	}
	if err := s.Drop(t.Context(), Damage{Segment: 1, HeadRow: 1}); !errors.Is(err, tinystore.ErrInvalid) {
		t.Errorf("a damage naming both: %v", err)
	}
	if c := s.counts(t); c.segments != 1 || c.heads != 1 {
		t.Fatalf("Drop removed what reads: %+v", c)
	}
}
