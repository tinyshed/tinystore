package spike

import (
	"database/sql"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/codec"
)

// the four classes a dense installation actually holds, kept apart because one
// average over them hides which one is expensive
var denseClasses = []string{"integers", "counter", "temperature", "noisy"}

type denseShare struct {
	name            string
	bytes, unused   int64
	rows            int64
	isIndex, isBody bool
}

type denseResult struct {
	class       string
	blocks      int
	samples     int
	payload     int64
	file        int64
	shares      []denseShare
	summaryScan time.Duration
	pointQuery  time.Duration
}

func (r denseResult) perSample(bytes int64) float64 { return float64(bytes) / float64(r.samples) }

// where the file went, in the four lines a decision is made from
func (r denseResult) lines() (payload, metadata, indexes, waste float64) {
	payload = r.perSample(r.payload)
	for _, share := range r.shares {
		switch {
		case share.isIndex:
			indexes += r.perSample(share.bytes - share.unused)
		case share.isBody:
			metadata += r.perSample(share.bytes - share.unused - r.payload)
		default:
			metadata += r.perSample(share.bytes - share.unused)
		}
		waste += r.perSample(share.unused)
	}
	return payload, metadata - payload + payload, indexes, waste
}

type denseSchema struct {
	name    string
	create  []string
	indexes []string
	insert  func(tx *sql.Tx, i int, seriesID int64, s summary, packed []byte) error
	body    string
}

func measureDense(t *testing.T, class string, blocks, perBlock int, schema denseSchema,
	encode func(samples []codec.Sample) ([]byte, error),
) denseResult {
	t.Helper()

	db := openNight(t, filepath.Join(t.TempDir(), "night.db"), schema)
	defer func() {
		if err := db.Close(); err != nil {
			t.Error(err)
		}
	}()

	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	result := denseResult{class: class, blocks: blocks, samples: blocks * perBlock}
	for i := range blocks {
		samples := adaptiveSamples(class, perBlock, i)
		old := make([]sample, len(samples))
		for j, s := range samples {
			old[j] = sample{at: s.At, value: s.Value}
		}
		packed, encodeErr := encode(samples)
		if encodeErr != nil {
			t.Fatal(encodeErr)
		}
		result.payload += int64(len(packed))
		if err = schema.insert(tx, i, int64(i%1000)+1, summarise(old), packed); err != nil {
			t.Fatal(err)
		}
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
	checkpoint(t, db)

	result.shares, result.file = nightShares(t, db, schema.body)
	result.summaryScan = timeNight(t, db, `select sum(count), sum(sum) from blocks`)
	result.pointQuery = timeNight(t, db,
		`select count(*) from blocks where series_id = 7 and start_ts >= 0`)
	return result
}

func openNight(t *testing.T, path string, schema denseSchema) *sql.DB {
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
	for _, statement := range append(append([]string{}, schema.create...), schema.indexes...) {
		if _, err = db.Exec(statement); err != nil {
			t.Fatalf("%s: %v", schema.name, err)
		}
	}
	return db
}

// dbstat reports each b-tree's pages, what they hold and what they waste
func nightShares(t *testing.T, db *sql.DB, body string) ([]denseShare, int64) {
	t.Helper()

	rows, err := db.Query(`select name, sum(pgsize), sum(unused), sum(ncell) from dbstat group by name`)
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()

	var shares []denseShare
	var total int64
	for rows.Next() {
		var share denseShare
		if err = rows.Scan(&share.name, &share.bytes, &share.unused, &share.rows); err != nil {
			t.Fatal(err)
		}
		share.isIndex = share.name != "blocks" && share.name != "payloads" && share.name != "sqlite_schema"
		share.isBody = share.name == body
		shares = append(shares, share)
		total += share.bytes
	}
	if err = rows.Err(); err != nil {
		t.Fatal(err)
	}
	sort.Slice(shares, func(i, j int) bool { return shares[i].bytes > shares[j].bytes })
	return shares, total
}

func timeNight(t *testing.T, db *sql.DB, query string) time.Duration {
	t.Helper()

	const runs = 20
	start := time.Now()
	for range runs {
		var a, b sql.NullFloat64
		row := db.QueryRow(query)
		if err := row.Scan(&a, &b); err != nil {
			if err = db.QueryRow(query).Scan(&a); err != nil {
				t.Fatal(err)
			}
		}
	}
	return time.Since(start) / runs
}

func reportDense(t *testing.T, results []denseResult) {
	t.Helper()

	var payloadSum, metadataSum, indexSum, wasteSum float64
	for _, result := range results {
		payload, metadata, indexes, waste := result.lines()
		payloadSum += payload
		metadataSum += metadata
		indexSum += indexes
		wasteSum += waste
		t.Logf("%-12s total=%.3f  payload=%.3f  metadata=%.3f  indexes=%.3f  waste=%.3f  scan=%s  point=%s",
			result.class, result.perSample(result.file), payload, metadata, indexes, waste,
			result.summaryScan, result.pointQuery)
		for _, share := range result.shares {
			if share.bytes > result.file/200 {
				t.Logf("        %-14s %7.3f B/sample  %5.1f%% unused",
					share.name, result.perSample(share.bytes),
					100*float64(share.unused)/float64(share.bytes))
			}
		}
	}
	n := float64(len(results))
	t.Logf("%-12s total=%.3f  payload=%.3f  metadata=%.3f  indexes=%.3f  waste=%.3f",
		"MIXED", (payloadSum+metadataSum+indexSum+wasteSum)/n, payloadSum/n, metadataSum/n, indexSum/n, wasteSum/n)
}

// the schema as it stands today: a clustered block table, a payload table and
// the two maintenance indexes
func todaysSchema() denseSchema {
	return denseSchema{
		name: "as measured",
		create: []string{
			`create table payloads (id integer primary key, body blob not null) strict`,
			`create table blocks (series_id integer not null, start_ts integer not null,
			  end_ts integer not null, count integer not null,
			  min real, max real, sum real, first real, last real, increase real,
			  resets integer not null,
			  payload_id integer references payloads(id) on delete set null,
			  primary key (series_id, start_ts)) strict, without rowid`,
		},
		indexes: []string{
			`create index block_expiry on blocks(end_ts)`,
			`create index block_payload on blocks(payload_id)`,
		},
		body: "payloads",
		insert: func(tx *sql.Tx, i int, seriesID int64, s summary, packed []byte) error {
			if _, err := tx.Exec(`insert into payloads(id, body) values(?,?)`, i+1, packed); err != nil {
				return fmt.Errorf("payload: %w", err)
			}
			_, err := tx.Exec(`insert into blocks values(?,?,?,?,?,?,?,?,?,?,?,?)`,
				seriesID, s.startTS, s.endTS, s.count, s.min, s.max, s.sum,
				s.first, s.last, s.increase, s.resets, i+1)
			return err //nolint:wrapcheck // the caller names the schema
		},
	}
}

func TestTonightsBaseline(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	blocks, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer blocks.Close()

	results := make([]denseResult, 0, len(denseClasses))
	for _, class := range denseClasses {
		results = append(results, measureDense(t, class, 10000, 240, todaysSchema(), blocks.Encode))
	}
	reportDense(t, results)
}
