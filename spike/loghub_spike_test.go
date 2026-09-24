package spike

import (
	"bytes"
	"database/sql"
	"encoding/binary"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/klauspost/compress/zstd"
)

const (
	loghubBlockLines   = 1000
	templateSimilarity = 0.5
	variableToken      = "<*>"
)

// logTemplate is one cluster of lines: the same length and the same constant words
type logTemplate struct {
	id     int
	tokens []string
}

// templateMiner is a small Drain: clusters by token count, then by the share of equal words
type templateMiner struct {
	byLength map[int][]*logTemplate
	all      []*logTemplate
}

// lineTokens splits on single spaces, so joining them gives the line back byte for byte
func lineTokens(line string) []string { return strings.Split(line, " ") }

func hasDigit(token string) bool { return strings.ContainsAny(token, "0123456789") }

// add returns the template a line joins, generalising it where the line differs
//
//	"open file 7 ok" + "open file 9 ok" → "open file <*> ok"
func (m *templateMiner) add(line string) *logTemplate {
	tokens := lineTokens(line)
	masked := make([]string, len(tokens))
	for i, token := range tokens {
		masked[i] = token
		if hasDigit(token) {
			masked[i] = variableToken
		}
	}
	best, bestScore := (*logTemplate)(nil), 0.0
	for _, candidate := range m.byLength[len(tokens)] {
		if score := templateScore(candidate.tokens, masked); score > bestScore {
			best, bestScore = candidate, score
		}
	}
	if best == nil || bestScore < templateSimilarity {
		best = &logTemplate{id: len(m.all), tokens: masked}
		m.byLength[len(tokens)] = append(m.byLength[len(tokens)], best)
		m.all = append(m.all, best)
		return best
	}
	for i := range best.tokens {
		if best.tokens[i] != masked[i] {
			best.tokens[i] = variableToken
		}
	}
	return best
}

func templateScore(template, masked []string) float64 {
	if len(template) == 0 {
		return 1
	}
	equal := 0
	for i := range template {
		if template[i] == masked[i] {
			equal++
		}
	}
	return float64(equal) / float64(len(template))
}

// encodeTemplateBlock writes template ids, then every variable column of every template
func encodeTemplateBlock(lines []string, templates []*logTemplate) []byte {
	var ids []byte
	columns := map[[2]int]*bytes.Buffer{}
	var keys [][2]int
	for index, line := range lines {
		template := templates[index]
		ids = binary.AppendUvarint(ids, uint64(template.id))
		for position, token := range lineTokens(line) {
			if template.tokens[position] != variableToken {
				continue
			}
			key := [2]int{template.id, position}
			if columns[key] == nil {
				columns[key] = &bytes.Buffer{}
				keys = append(keys, key)
			}
			columns[key].WriteString(token + "\x00")
		}
	}
	slices.SortFunc(keys, func(a, b [2]int) int { return (a[0]-b[0])*1_000 + a[1] - b[1] })
	out := binary.AppendUvarint(nil, uint64(len(lines)))
	out = append(out, ids...)
	for _, key := range keys {
		out = append(out, columns[key].Bytes()...)
	}
	return out
}

// decodeTemplateBlock is the reverse, proving each line comes back byte for byte
func decodeTemplateBlock(body []byte, templates []*logTemplate) []string {
	count, width := binary.Uvarint(body)
	body = body[width:]
	ids := make([]int, count)
	for i := range ids {
		id, n := binary.Uvarint(body)
		ids[i], body = int(id), body[n:]
	}
	columns := map[[2]int][]string{}
	var keys [][2]int
	for _, id := range ids {
		for position, token := range templates[id].tokens {
			key := [2]int{id, position}
			if token == variableToken && columns[key] == nil {
				columns[key] = []string{}
				keys = append(keys, key)
			}
		}
	}
	slices.SortFunc(keys, func(a, b [2]int) int { return (a[0]-b[0])*1_000 + a[1] - b[1] })
	uses := map[[2]int]int{}
	for _, id := range ids {
		for position, token := range templates[id].tokens {
			if token == variableToken {
				uses[[2]int{id, position}]++
			}
		}
	}
	for _, key := range keys {
		for range uses[key] {
			end := bytes.IndexByte(body, 0)
			columns[key] = append(columns[key], string(body[:end]))
			body = body[end+1:]
		}
	}
	lines := make([]string, count)
	for i, id := range ids {
		tokens := slices.Clone(templates[id].tokens)
		for position, token := range tokens {
			if token == variableToken {
				key := [2]int{id, position}
				tokens[position], columns[key] = columns[key][0], columns[key][1:]
			}
		}
		lines[i] = strings.Join(tokens, " ")
	}
	return lines
}

type loghubResult struct {
	lines, rawBytes, templates    int
	plainPayload, templatePayload int
	plainFile, templateFile       int64
}

func TestLoghubTemplates(t *testing.T) {
	corpus := os.Getenv("TINYSTORE_LOGHUB")
	if os.Getenv("TINYSTORE_SPIKE") == "" || corpus == "" {
		t.Skip("set TINYSTORE_SPIKE=1 and TINYSTORE_LOGHUB=<corpus> to measure")
	}
	paths, err := filepath.Glob(filepath.Join(corpus, "*", "*_2k.log"))
	if err != nil || len(paths) == 0 {
		t.Fatalf("no loghub files under %s: %v", corpus, err)
	}
	t.Logf("%-12s %5s %9s %9s %9s %9s %9s %9s", "dataset", "lines", "templates",
		"raw B/l", "zstd B/l", "tmpl B/l", "zstd f/l", "tmpl f/l")
	for _, path := range paths {
		result := measureLoghubFile(t, path)
		per := func(n int64) float64 { return float64(n) / float64(result.lines) }
		t.Logf("%-12s %5d %9d %9.1f %9.1f %9.1f %9.1f %9.1f", filepath.Base(filepath.Dir(path)),
			result.lines, result.templates, per(int64(result.rawBytes)), per(int64(result.plainPayload)),
			per(int64(result.templatePayload)), per(result.plainFile), per(result.templateFile))
	}
}

func measureLoghubFile(t *testing.T, path string) loghubResult {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	lines := strings.Split(strings.TrimSuffix(string(data), "\n"), "\n")
	miner := &templateMiner{byLength: map[int][]*logTemplate{}}
	templates := make([]*logTemplate, len(lines))
	for i, line := range lines {
		templates[i] = miner.add(line)
	}
	encoder, err := zstd.NewWriter(nil, zstd.WithEncoderLevel(zstd.SpeedDefault))
	if err != nil {
		t.Fatal(err)
	}
	result := loghubResult{lines: len(lines), rawBytes: len(data), templates: len(miner.all)}
	var plainBlocks, templateBlocks [][]byte
	for start := 0; start < len(lines); start += loghubBlockLines {
		end := min(start+loghubBlockLines, len(lines))
		plain := encoder.EncodeAll([]byte(strings.Join(lines[start:end], "\n")), nil)
		body := encodeTemplateBlock(lines[start:end], templates[start:end])
		if !slices.Equal(decodeTemplateBlock(body, miner.all), lines[start:end]) {
			t.Fatalf("%s: a line did not come back byte for byte", path)
		}
		packed := encoder.EncodeAll(body, nil)
		plainBlocks, templateBlocks = append(plainBlocks, plain), append(templateBlocks, packed)
		result.plainPayload += len(plain)
		result.templatePayload += len(packed)
	}
	dictionary := encoder.EncodeAll([]byte(templateDictionary(miner.all)), nil)
	result.templatePayload += len(dictionary)
	result.plainFile = loghubFileSize(t, plainBlocks)
	result.templateFile = loghubFileSize(t, append(templateBlocks, dictionary))
	return result
}

func templateDictionary(templates []*logTemplate) string {
	var out strings.Builder
	for _, template := range templates {
		out.WriteString(strings.Join(template.tokens, " ") + "\n")
	}
	return out.String()
}

// loghubFileSize is what the blocks cost as rows of a vacuumed SQLite file
func loghubFileSize(t *testing.T, blocks [][]byte) int64 {
	t.Helper()
	ctx := t.Context()
	db, err := sql.Open("sqlite", filepath.Join(t.TempDir(), "blocks.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if _, err = db.ExecContext(ctx, `create table blocks (id integer primary key, body blob not null) strict`); err != nil {
		t.Fatal(err)
	}
	for _, block := range blocks {
		if _, err = db.ExecContext(ctx, `insert into blocks (body) values (?)`, block); err != nil {
			t.Fatal(err)
		}
	}
	if _, err = db.ExecContext(ctx, `vacuum`); err != nil {
		t.Fatal(err)
	}
	var size int64
	err = db.QueryRowContext(ctx, `select page_count * page_size from pragma_page_count, pragma_page_size`).Scan(&size)
	if err != nil {
		t.Fatal(err)
	}
	return size
}
