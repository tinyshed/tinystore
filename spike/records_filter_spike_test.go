package spike

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"hash/fnv"
	"os"
	"strings"
	"testing"
	"unicode"

	"github.com/klauspost/compress/zstd"
)

const (
	bloomBitsPerToken = 10
	bloomHashes       = 7
)

const filteredBlocksSchema = `create table blocks (id integer primary key, first_at integer not null,
	last_at integer not null, levels integer not null, bloom blob not null, body blob not null) strict;
create index blocks_at on blocks (first_at);`

// recordTokens splits like unicode61 with tokenchars '.:_-/' and folds case
func recordTokens(record logRecord) []string {
	isPart := func(r rune) bool {
		return unicode.IsLetter(r) || unicode.IsDigit(r) || strings.ContainsRune(".:_-/", r)
	}
	text := record.Message
	for _, value := range record.Attrs {
		text += " " + fmt.Sprint(value)
	}
	return strings.FieldsFunc(strings.ToLower(text), func(r rune) bool { return !isPart(r) })
}

func blockTokens(block []logRecord) map[string]bool {
	tokens := map[string]bool{}
	for _, record := range block {
		for _, token := range recordTokens(record) {
			tokens[token] = true
		}
	}
	return tokens
}

// levelBit maps slog's -4, 0, 4, 8 to bits 0..3
func levelBit(level int) int64 { return 1 << ((level + 4) / 4) }

func bloomPositions(token string, bits uint64, visit func(uint64)) {
	first, second := fnv.New64a(), fnv.New64()
	_, _ = first.Write([]byte(token))
	_, _ = second.Write([]byte(token))
	a, b := first.Sum64(), second.Sum64()|1
	for i := range uint64(bloomHashes) {
		visit((a + i*b) % bits)
	}
}

func buildBloom(tokens map[string]bool) []byte {
	filter := make([]byte, max(8, (len(tokens)*bloomBitsPerToken+7)/8))
	for token := range tokens {
		bloomPositions(token, uint64(len(filter))*8, func(bit uint64) { filter[bit/8] |= 1 << (bit % 8) })
	}
	return filter
}

func bloomMayHold(filter []byte, token string) bool {
	held := true
	bloomPositions(token, uint64(len(filter))*8, func(bit uint64) { held = held && filter[bit/8]&(1<<(bit%8)) != 0 })
	return held
}

// writeFilteredBlocks packs blocks as writeBlocks does, with a level mask and a token bloom filter each
func writeFilteredBlocks(size int) func(context.Context, *sql.Tx, []logRecord) (int, error) {
	return func(ctx context.Context, tx *sql.Tx, records []logRecord) (int, error) {
		if _, err := tx.ExecContext(ctx, filteredBlocksSchema); err != nil {
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
			var levels int64
			for _, record := range block {
				line, _ := json.Marshal(record)
				lines.Write(append(line, '\n'))
				levels |= levelBit(record.Level)
			}
			body := encoder.EncodeAll(lines.Bytes(), nil)
			bloom := buildBloom(blockTokens(block))
			payload += len(body) + len(bloom) + 24
			_, err = tx.ExecContext(ctx,
				`insert into blocks (first_at, last_at, levels, bloom, body) values (?, ?, ?, ?, ?)`,
				block[0].At, block[len(block)-1].At, levels, bloom, body)
			if err != nil {
				return 0, err
			}
		}
		return payload, nil
	}
}

type blockFilter struct {
	levels int64
	bloom  []byte
}

func readBlockFilters(t *testing.T, db *sql.DB) []blockFilter {
	t.Helper()
	rows, err := db.QueryContext(t.Context(), `select levels, bloom from blocks order by id`)
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()
	var filters []blockFilter
	for rows.Next() {
		var filter blockFilter
		if err = rows.Scan(&filter.levels, &filter.bloom); err != nil {
			t.Fatal(err)
		}
		filters = append(filters, filter)
	}
	if err = rows.Err(); err != nil {
		t.Fatal(err)
	}
	return filters
}

func TestRecordBlockFilters(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	const count = 100_000
	records := syntheticLogs(count)
	rare := records[len(records)/2].Attrs["trace_id"].(string)
	user := records[len(records)/3].Attrs["user"].(string)
	terms := []string{rare, user, "refused", "/api/export", "absent-token"}
	for _, size := range []int{256, 1024} {
		layout := recordLayout{fmt.Sprintf("blocks of %d + levels + bloom", size), writeFilteredBlocks(size)}
		db, payload, file, objects := openRecordLayout(t, layout, records)
		t.Logf("%-34s payload/r %.1f file/r %.1f  %s", layout.name,
			float64(payload)/count, float64(file)/count, objects)
		filters := readBlockFilters(t, db)
		for _, term := range terms {
			candidates, holding := 0, 0
			for index, filter := range filters {
				if bloomMayHold(filter.bloom, term) {
					candidates++
				}
				if blockTokens(records[index*size : min((index+1)*size, count)])[term] {
					holding++
				}
			}
			t.Logf("  %-18s candidates %4d of %d, holding %4d", term, candidates, len(filters), holding)
		}
		errors := 0
		for _, filter := range filters {
			if filter.levels&levelBit(8) != 0 {
				errors++
			}
		}
		t.Logf("  %-18s candidates %4d of %d", "level >= error", errors, len(filters))
	}
}
