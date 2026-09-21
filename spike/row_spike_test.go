package spike

import (
	"database/sql"
	"fmt"
	"os"
	"testing"

	"github.com/tinyshed/tinystore/codec"
)

// a gauge has no increase and a counter no sum, and a span is smaller than the
// absolute timestamp it is the distance to
func squeezedSchema(name string, nullUnused, spanNotEnd, counter bool) denseSchema {
	end := "end_ts integer not null"
	if spanNotEnd {
		end = "span integer not null"
	}
	schema := denseSchema{
		name: name,
		create: []string{
			`create table payloads (id integer primary key, body blob not null) strict`,
			`create table blocks (series_id integer not null, start_ts integer not null,
			  ` + end + `, count integer not null,
			  min real, max real, sum real, first real, last real, increase real,
			  resets integer, payload_id integer not null,
			  primary key (series_id, start_ts)) strict, without rowid`,
			seriesStateTable,
		},
		indexes: []string{
			`create index series_due on series_state(next_gc_ts) where next_gc_ts is not null`,
		},
		body: "payloads",
	}
	schema.insert = func(tx *sql.Tx, i int, seriesID int64, s summary, packed []byte) error {
		if _, err := tx.Exec(`insert into payloads(id, body) values(?,?)`, i+1, packed); err != nil {
			return fmt.Errorf("payload: %w", err)
		}
		last := s.endTS
		if spanNotEnd {
			last = s.endTS - s.startTS
		}
		var low, high, total, increase, resets any = s.min, s.max, s.sum, s.increase, s.resets
		if nullUnused {
			if counter {
				low, high, total = nil, nil, nil
			} else {
				increase, resets = nil, nil
			}
		}
		if _, err := tx.Exec(`insert into blocks values(?,?,?,?,?,?,?,?,?,?,?,?)`,
			seriesID, s.startTS, last, s.count, low, high, total,
			s.first, s.last, increase, resets, i+1); err != nil {
			return fmt.Errorf("block: %w", err)
		}
		return upsertState(tx, seriesID, s)
	}
	return schema
}

func TestWhatIsLeftInTheBlockRow(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	blocks, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer blocks.Close()

	for _, class := range denseClasses {
		for _, schema := range []denseSchema{
			squeezedSchema("tonight, for comparison", false, false, class == "counter"),
			squeezedSchema("unused summary columns null", true, false, class == "counter"),
			squeezedSchema("a span instead of the last timestamp", false, true, class == "counter"),
			squeezedSchema("both", true, true, class == "counter"),
		} {
			result := measureDense(t, class, 10000, 240, schema, blocks.Encode)
			payload, metadata, indexes, waste := result.lines()
			t.Logf("%-12s %-38s total=%.3f  payload=%.3f  metadata=%.3f  indexes=%.3f  waste=%.3f",
				class, schema.name, result.perSample(result.file), payload, metadata, indexes, waste)
		}
	}
}
