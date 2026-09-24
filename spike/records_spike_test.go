package spike

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"math/rand/v2"
	"os"
	"path/filepath"
	"testing"

	"github.com/klauspost/compress/zstd"
	_ "modernc.org/sqlite"
)

// logRecord is one synthetic line of a small web service
type logRecord struct {
	At      int64
	Level   int
	Message string
	Attrs   map[string]any
}

var logTemplates = []struct {
	level   int
	message string
}{
	{0, "request served"},
	{0, "request served"},
	{0, "request served"},
	{0, "request served"},
	{0, "cache hit"},
	{0, "cache miss"},
	{-4, "query planned"},
	{-4, "connection reused"},
	{0, "user signed in"},
	{0, "note saved"},
	{4, "slow request"},
	{4, "retrying upstream"},
	{8, "upstream failed"},
	{0, "job finished"},
	{-4, "flush scheduled"},
	{0, "session expired"},
}

var logRoutes = []string{"/", "/notes", "/notes/:id", "/login", "/logout", "/api/search", "/api/export", "/health"}

// syntheticLogs is deterministic: the same seed gives the same corpus
func syntheticLogs(n int) []logRecord {
	random := rand.New(rand.NewPCG(1, 2))
	at := int64(1_790_000_000_000)
	out := make([]logRecord, n)
	for i := range out {
		at += int64(random.IntN(200))
		template := logTemplates[random.IntN(len(logTemplates))]
		attrs := map[string]any{
			"route":    logRoutes[random.IntN(len(logRoutes))],
			"status":   []int{200, 200, 200, 204, 302, 404, 500}[random.IntN(7)],
			"ms":       random.IntN(400),
			"user":     fmt.Sprintf("u%05d", random.IntN(5000)),
			"trace_id": fmt.Sprintf("%016x", random.Uint64()),
		}
		if template.level == 8 {
			attrs["error"] = "dial tcp 10.0.0.7:5432: connect: connection refused"
		}
		out[i] = logRecord{At: at, Level: template.level, Message: template.message, Attrs: attrs}
	}
	return out
}

type recordLayout struct {
	name  string
	write func(ctx context.Context, tx *sql.Tx, records []logRecord) (payload int, err error)
}

var recordLayouts = []recordLayout{
	{"rows (records today)", writeRows(`create table records (id integer primary key, at integer not null,
		level integer not null, message text not null, attrs text not null) strict;
		create index records_at on records (at);`)},
	{"rows + fts5 on message", writeRows(`create table records (id integer primary key, at integer not null,
		level integer not null, message text not null, attrs text not null) strict;
		create index records_at on records (at);
		create virtual table records_text using fts5(message, attrs, content='records', content_rowid='id');
		create trigger records_ai after insert on records begin
			insert into records_text (rowid, message, attrs) values (new.id, new.message, new.attrs); end;`)},
	{"rows + fts5 trigram", writeRows(`create table records (id integer primary key, at integer not null,
		level integer not null, message text not null, attrs text not null) strict;
		create index records_at on records (at);
		create virtual table records_text using fts5(message, attrs, content='records', content_rowid='id',
			tokenize='trigram');
		create trigger records_ai after insert on records begin
			insert into records_text (rowid, message, attrs) values (new.id, new.message, new.attrs); end;`)},
	{"dictionary for messages and keys", writeDictionary},
	{"zstd blocks of 256 records", writeBlocks(256)},
	{"zstd blocks of 1024 records", writeBlocks(1024)},
}

func writeRows(schema string) func(context.Context, *sql.Tx, []logRecord) (int, error) {
	return func(ctx context.Context, tx *sql.Tx, records []logRecord) (int, error) {
		if _, err := tx.ExecContext(ctx, schema); err != nil {
			return 0, err
		}
		payload := 0
		for _, record := range records {
			attrs, _ := json.Marshal(record.Attrs)
			payload += len(record.Message) + len(attrs) + 9
			_, err := tx.ExecContext(ctx, `insert into records (at, level, message, attrs) values (?, ?, ?, ?)`,
				record.At, record.Level, record.Message, string(attrs))
			if err != nil {
				return 0, err
			}
		}
		return payload, nil
	}
}

const dictionarySchema = `create table words (id integer primary key, text text not null unique) strict;
create table records (id integer primary key, at integer not null, level integer not null,
	message integer not null, attrs blob not null) strict;
create index records_at on records (at);`

// writeDictionary stores a message and every attribute key as an id, and values as JSON
func writeDictionary(ctx context.Context, tx *sql.Tx, records []logRecord) (int, error) {
	if _, err := tx.ExecContext(ctx, dictionarySchema); err != nil {
		return 0, err
	}
	words := map[string]int{}
	word := func(text string) (int, error) {
		if id, ok := words[text]; ok {
			return id, nil
		}
		words[text] = len(words) + 1
		_, err := tx.ExecContext(ctx, `insert into words (id, text) values (?, ?)`, len(words), text)
		return len(words), err
	}
	payload := 0
	for _, record := range records {
		message, err := word(record.Message)
		if err != nil {
			return 0, err
		}
		var attrs []any
		for _, key := range []string{"route", "status", "ms", "user", "trace_id", "error"} {
			if value, ok := record.Attrs[key]; ok {
				id, keyErr := word(key)
				if keyErr != nil {
					return 0, keyErr
				}
				attrs = append(attrs, id, value)
			}
		}
		encoded, _ := json.Marshal(attrs)
		payload += len(encoded) + 12
		_, err = tx.ExecContext(ctx, `insert into records (at, level, message, attrs) values (?, ?, ?, ?)`,
			record.At, record.Level, message, encoded)
		if err != nil {
			return 0, err
		}
	}
	return payload, nil
}

// writeBlocks packs size records as JSON lines under one zstd frame per row
func writeBlocks(size int) func(context.Context, *sql.Tx, []logRecord) (int, error) {
	return func(ctx context.Context, tx *sql.Tx, records []logRecord) (int, error) {
		_, err := tx.ExecContext(ctx, `create table blocks (id integer primary key, first_at integer not null,
			last_at integer not null, body blob not null) strict; create index blocks_at on blocks (first_at);`)
		if err != nil {
			return 0, err
		}
		encoder, err := zstd.NewWriter(nil, zstd.WithEncoderLevel(zstd.SpeedDefault))
		if err != nil {
			return 0, err
		}
		payload := 0
		for start := 0; start < len(records); start += size {
			block := records[start:min(start+size, len(records))]
			var lines bytes.Buffer
			for _, record := range block {
				line, _ := json.Marshal(record)
				lines.Write(append(line, '\n'))
			}
			body := encoder.EncodeAll(lines.Bytes(), nil)
			payload += len(body) + 16
			_, err = tx.ExecContext(ctx, `insert into blocks (first_at, last_at, body) values (?, ?, ?)`,
				block[0].At, block[len(block)-1].At, body)
			if err != nil {
				return 0, err
			}
		}
		return payload, nil
	}
}

func TestRecordLayoutDensity(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	const count = 100_000
	records := syntheticLogs(count)
	t.Logf("%d synthetic records, %d messages, %d routes", count, len(logTemplates), len(logRoutes))
	t.Logf("%-34s %10s %10s  objects", "layout", "payload/r", "file/r")
	for _, layout := range recordLayouts {
		payload, file, objects := measureRecordLayout(t, layout, records)
		t.Logf("%-34s %10.1f %10.1f  %s", layout.name,
			float64(payload)/count, float64(file)/count, objects)
	}
}

func measureRecordLayout(t *testing.T, layout recordLayout, records []logRecord) (int, int64, string) {
	t.Helper()
	ctx := t.Context()
	db, err := sql.Open("sqlite", filepath.Join(t.TempDir(), "records.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		t.Fatal(err)
	}
	payload, err := layout.write(ctx, tx, records)
	if err != nil {
		t.Fatalf("%s: %v", layout.name, err)
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
	if _, err = db.ExecContext(ctx, `vacuum`); err != nil {
		t.Fatal(err)
	}
	var file int64
	err = db.QueryRowContext(ctx, `select page_count * page_size from pragma_page_count, pragma_page_size`).Scan(&file)
	if err != nil {
		t.Fatal(err)
	}
	return payload, file, recordObjects(t, db)
}

// recordObjects is the file divided by b-tree, in KiB, largest first
func recordObjects(t *testing.T, db *sql.DB) string {
	t.Helper()
	rows, err := db.QueryContext(t.Context(),
		`select name, sum(pgsize) / 1024 from dbstat group by name order by 2 desc`)
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()
	var out string
	for rows.Next() {
		var name string
		var kib int64
		if err = rows.Scan(&name, &kib); err != nil {
			t.Fatal(err)
		}
		if kib > 0 {
			out += fmt.Sprintf("%s=%d ", name, kib)
		}
	}
	if err = rows.Err(); err != nil {
		t.Fatal(err)
	}
	return out
}
