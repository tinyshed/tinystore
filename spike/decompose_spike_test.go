package spike

import (
	"database/sql"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/codec"
)

// the layouts differ only in what the block row carries and where the body lives
var denseLayouts = []struct {
	name    string
	blocks  string
	payload string
	indexes []string
}{
	{
		name: "as measured",
		blocks: `create table blocks (series_id integer not null, start_ts integer not null,
		  end_ts integer not null, count integer not null,
		  min real, max real, sum real, first real, last real, increase real,
		  resets integer not null,
		  payload_id integer references payloads(id) on delete set null,
		  primary key (series_id, start_ts)) strict, without rowid`,
		payload: `create table payloads (id integer primary key, body blob not null) strict`,
		indexes: []string{
			`create index block_expiry on blocks(end_ts)`,
			`create index block_payload on blocks(payload_id)`,
		},
	},
	{
		name: "summary as integers",
		blocks: `create table blocks (series_id integer not null, start_ts integer not null,
		  end_ts integer not null, count integer not null,
		  min integer, max integer, sum integer, first integer, last integer, increase integer,
		  resets integer not null,
		  payload_id integer references payloads(id) on delete set null,
		  primary key (series_id, start_ts)) strict, without rowid`,
		payload: `create table payloads (id integer primary key, body blob not null) strict`,
		indexes: []string{
			`create index block_expiry on blocks(end_ts)`,
			`create index block_payload on blocks(payload_id)`,
		},
	},
	{
		name: "no summary",
		blocks: `create table blocks (series_id integer not null, start_ts integer not null,
		  end_ts integer not null, count integer not null,
		  payload_id integer references payloads(id) on delete set null,
		  primary key (series_id, start_ts)) strict, without rowid`,
		payload: `create table payloads (id integer primary key, body blob not null) strict`,
		indexes: []string{
			`create index block_expiry on blocks(end_ts)`,
			`create index block_payload on blocks(payload_id)`,
		},
	},
	{
		name: "no maintenance indexes",
		blocks: `create table blocks (series_id integer not null, start_ts integer not null,
		  end_ts integer not null, count integer not null,
		  min real, max real, sum real, first real, last real, increase real,
		  resets integer not null,
		  payload_id integer references payloads(id) on delete set null,
		  primary key (series_id, start_ts)) strict, without rowid`,
		payload: `create table payloads (id integer primary key, body blob not null) strict`,
	},
	{
		name: "body in the block row",
		blocks: `create table blocks (series_id integer not null, start_ts integer not null,
		  end_ts integer not null, count integer not null,
		  min real, max real, sum real, first real, last real, increase real,
		  resets integer not null, body blob not null,
		  primary key (series_id, start_ts)) strict, without rowid`,
		indexes: []string{`create index block_expiry on blocks(end_ts)`},
	},
	{
		name: "payload keyed by the block",
		blocks: `create table blocks (series_id integer not null, start_ts integer not null,
		  end_ts integer not null, count integer not null,
		  min real, max real, sum real, first real, last real, increase real,
		  resets integer not null,
		  primary key (series_id, start_ts)) strict, without rowid`,
		payload: `create table payloads (series_id integer not null, start_ts integer not null,
		  body blob not null,
		  primary key (series_id, start_ts),
		  foreign key (series_id, start_ts) references blocks(series_id, start_ts) on delete cascade) strict, without rowid`,
		indexes: []string{`create index block_expiry on blocks(end_ts)`},
	},
	{
		name: "the same, expiry by series",
		blocks: `create table blocks (series_id integer not null, start_ts integer not null,
		  end_ts integer not null, count integer not null,
		  min real, max real, sum real, first real, last real, increase real,
		  resets integer not null,
		  primary key (series_id, start_ts)) strict, without rowid`,
		payload: `create table payloads (series_id integer not null, start_ts integer not null,
		  body blob not null,
		  primary key (series_id, start_ts),
		  foreign key (series_id, start_ts) references blocks(series_id, start_ts) on delete cascade) strict, without rowid`,
	},
	{
		name: "body inline, expiry by series",
		blocks: `create table blocks (series_id integer not null, start_ts integer not null,
		  end_ts integer not null, count integer not null,
		  min real, max real, sum real, first real, last real, increase real,
		  resets integer not null, body blob not null,
		  primary key (series_id, start_ts)) strict, without rowid`,
	},
	{
		name: "floor",
		blocks: `create table blocks (series_id integer not null, start_ts integer not null,
		  body blob not null, primary key (series_id, start_ts)) strict, without rowid`,
	},
}

func TestWhereADenseBlocksBytesAre(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	blocks, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer blocks.Close()

	const count, perBlock = 10000, 240
	for _, layout := range denseLayouts {
		db := openDecomposed(t, filepath.Join(t.TempDir(), "decompose.db"), layout.blocks, layout.payload, layout.indexes)

		tx, beginErr := db.Begin()
		if beginErr != nil {
			t.Fatal(beginErr)
		}
		payloadBytes := 0
		for i := range count {
			samples := adaptiveSamples("integers", perBlock, i)
			old := make([]sample, len(samples))
			for j, s := range samples {
				old[j] = sample{at: s.At, value: s.Value}
			}
			packed, encodeErr := blocks.Encode(samples)
			if encodeErr != nil {
				t.Fatal(encodeErr)
			}
			payloadBytes += len(packed)
			writeDecomposed(t, tx, layout.name, layout.payload != "", i, summarise(old), packed)
		}
		if err = tx.Commit(); err != nil {
			t.Fatal(err)
		}
		checkpoint(t, db)

		pages, physical := pagesByObject(t, db)
		samples := float64(count * perBlock)
		t.Logf("%-27s file=%.3f B/sample  payload=%.3f  overhead=%.1f B/block  summary scan=%s",
			layout.name, float64(physical)/samples, float64(payloadBytes)/samples,
			float64(physical-int64(payloadBytes))/count, summaryScan(t, db, count*perBlock))
		for _, object := range sortedObjects(pages) {
			t.Logf("    %-16s %7.1f B/block  %6.3f B/sample", object, float64(pages[object])/count, float64(pages[object])/samples)
		}
		if err = db.Close(); err != nil {
			t.Fatal(err)
		}
	}
}

// a whole-block query reads these columns and no payload, which is what they exist for
func summaryScan(t *testing.T, db *sql.DB, want int) string {
	t.Helper()

	var probe int
	if err := db.QueryRow(`select count(*) from pragma_table_info('blocks') where name = 'sum'`).Scan(&probe); err != nil {
		t.Fatal(err)
	}
	if probe == 0 {
		return "n/a"
	}
	start := time.Now()
	const runs = 20
	for range runs {
		var n int
		var total float64
		if err := db.QueryRow(`select sum(count), sum(sum) from blocks`).Scan(&n, &total); err != nil {
			t.Fatal(err)
		}
		if n != want {
			t.Fatal("summary lost samples")
		}
	}
	return (time.Since(start) / runs).String()
}

func openDecomposed(t *testing.T, path, blocks, payload string, indexes []string) *sql.DB {
	t.Helper()

	dsn := "file:" + path + "?_dqs=0&_defensive=1&_pragma=busy_timeout(5000)" +
		"&_pragma=synchronous(NORMAL)&_pragma=temp_store(MEMORY)&_txlock=immediate" +
		"&_pragma=journal_mode(WAL)&_pragma=foreign_keys(1)"
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		t.Fatal(err)
	}
	db.SetMaxOpenConns(1)
	db.SetConnMaxLifetime(0)

	statements := make([]string, 0, len(indexes)+2)
	if payload != "" {
		statements = append(statements, payload)
	}
	statements = append(append(statements, blocks), indexes...)
	for _, statement := range statements {
		if _, err = db.Exec(statement); err != nil {
			t.Fatal(err)
		}
	}
	return db
}

func writeDecomposed(t *testing.T, tx *sql.Tx, layout string, separate bool, i int, s summary, packed []byte) {
	t.Helper()

	seriesID := i%1000 + 1
	composite := strings.HasPrefix(layout, "payload keyed") || strings.HasPrefix(layout, "the same")
	switch {
	case layout == "floor":
		if _, err := tx.Exec(`insert into blocks values(?,?,?)`, seriesID, s.startTS, packed); err != nil {
			t.Fatal(err)
		}
	case layout == "no summary":
		if _, err := tx.Exec(`insert into payloads(id, body) values(?,?)`, i+1, packed); err != nil {
			t.Fatal(err)
		}
		if _, err := tx.Exec(`insert into blocks values(?,?,?,?,?)`,
			seriesID, s.startTS, s.endTS, s.count, i+1); err != nil {
			t.Fatal(err)
		}
	case !separate:
		if _, err := tx.Exec(`insert into blocks values(?,?,?,?,?,?,?,?,?,?,?,?)`,
			seriesID, s.startTS, s.endTS, s.count, s.min, s.max, s.sum,
			s.first, s.last, s.increase, s.resets, packed); err != nil {
			t.Fatal(err)
		}
	case composite:
		if _, err := tx.Exec(`insert into blocks values(?,?,?,?,?,?,?,?,?,?,?)`,
			seriesID, s.startTS, s.endTS, s.count, s.min, s.max, s.sum,
			s.first, s.last, s.increase, s.resets); err != nil {
			t.Fatal(err)
		}
		if _, err := tx.Exec(`insert into payloads values(?,?,?)`, seriesID, s.startTS, packed); err != nil {
			t.Fatal(err)
		}
	default:
		if _, err := tx.Exec(`insert into payloads(id, body) values(?,?)`, i+1, packed); err != nil {
			t.Fatal(err)
		}
		if _, err := tx.Exec(`insert into blocks values(?,?,?,?,?,?,?,?,?,?,?,?)`,
			seriesID, s.startTS, s.endTS, s.count, s.min, s.max, s.sum,
			s.first, s.last, s.increase, s.resets, i+1); err != nil {
			t.Fatal(err)
		}
	}
}

// dbstat counts a b-tree's own pages, so a table's overflow pages land on the table
func pagesByObject(t *testing.T, db *sql.DB) (map[string]int64, int64) {
	t.Helper()

	rows, err := db.Query(`select name, sum(pgsize) from dbstat group by name`)
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()

	pages := map[string]int64{}
	var total int64
	for rows.Next() {
		var name string
		var size int64
		if err = rows.Scan(&name, &size); err != nil {
			t.Fatal(err)
		}
		pages[name] = size
		total += size
	}
	if err = rows.Err(); err != nil {
		t.Fatal(err)
	}

	var pageSize, pageCount int64
	if err = db.QueryRow(`pragma page_size`).Scan(&pageSize); err != nil {
		t.Fatal(err)
	}
	if err = db.QueryRow(`pragma page_count`).Scan(&pageCount); err != nil {
		t.Fatal(err)
	}
	if physical := pageSize * pageCount; physical != total {
		pages["unaccounted"] = physical - total
		return pages, physical
	}
	return pages, total
}

func sortedObjects(pages map[string]int64) []string {
	names := make([]string, 0, len(pages))
	for name := range pages {
		names = append(names, name)
	}
	sort.Slice(names, func(i, j int) bool { return pages[names[i]] > pages[names[j]] })
	return names
}

func TestWhatASummaryCostsWhenItIsNotWholeNumbers(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	blocks, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer blocks.Close()

	const count, perBlock = 10000, 240
	for _, kind := range []string{"integers", "temperature", "noisy"} {
		for _, layout := range denseLayouts {
			if layout.name != "as measured" && layout.name != "no summary" {
				continue
			}
			db := openDecomposed(t, filepath.Join(t.TempDir(), "summary.db"), layout.blocks, layout.payload, layout.indexes)
			tx, beginErr := db.Begin()
			if beginErr != nil {
				t.Fatal(beginErr)
			}
			payloadBytes := 0
			for i := range count {
				samples := adaptiveSamples(kind, perBlock, i)
				old := make([]sample, len(samples))
				for j, s := range samples {
					old[j] = sample{at: s.At, value: s.Value}
				}
				packed, encodeErr := blocks.Encode(samples)
				if encodeErr != nil {
					t.Fatal(encodeErr)
				}
				payloadBytes += len(packed)
				writeDecomposed(t, tx, layout.name, layout.payload != "", i, summarise(old), packed)
			}
			if err = tx.Commit(); err != nil {
				t.Fatal(err)
			}
			checkpoint(t, db)
			pages, physical := pagesByObject(t, db)
			t.Logf("%-12s %-12s file=%.3f B/sample  payload=%.3f  blocks=%.1f B/block",
				kind, layout.name, float64(physical)/float64(count*perBlock),
				float64(payloadBytes)/float64(count*perBlock), float64(pages["blocks"])/count)
			if err = db.Close(); err != nil {
				t.Fatal(err)
			}
		}
	}
}
