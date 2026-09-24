package spike

import (
	"bytes"
	"cmp"
	"database/sql"
	"encoding/binary"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strconv"
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
			columns[key].Write(appendString(nil, token))
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
			var value string
			value, body = readString(body)
			columns[key] = append(columns[key], value)
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
	bestPayload, typedPayload     int
	plainFile, templateFile       int64
	typedFile                     int64
}

func TestLoghubTemplates(t *testing.T) {
	corpus := os.Getenv("TINYSTORE_LOGHUB")
	if os.Getenv("TINYSTORE_SPIKE") == "" || corpus == "" {
		t.Skip("set TINYSTORE_SPIKE=1 and TINYSTORE_LOGHUB=<corpus> to measure")
	}
	paths, err := filepath.Glob(filepath.Join(corpus, "*", "*_"+cmp.Or(os.Getenv("TINYSTORE_LOGHUB_SET"), "2k")+".log"))
	if err != nil || len(paths) == 0 {
		t.Fatalf("no loghub files under %s: %v", corpus, err)
	}
	t.Logf("%-12s %5s %5s %7s %7s %7s %7s %7s %7s %7s %7s", "dataset", "lines", "tmpls",
		"raw", "zstd", "zstd19", "tmpl", "typed", "zstd f", "tmpl f", "typed f")
	var sum loghubResult
	for _, path := range paths {
		result := measureLoghubFile(t, path)
		per := func(n int64) float64 { return float64(n) / float64(result.lines) }
		t.Logf("%-12s %5d %5d %7.1f %7.1f %7.1f %7.1f %7.1f %7.1f %7.1f %7.1f", filepath.Base(filepath.Dir(path)),
			result.lines, result.templates, per(int64(result.rawBytes)), per(int64(result.plainPayload)),
			per(int64(result.bestPayload)), per(int64(result.templatePayload)), per(int64(result.typedPayload)),
			per(result.plainFile), per(result.templateFile), per(result.typedFile))
		sum.lines += result.lines
		sum.rawBytes += result.rawBytes
		sum.plainPayload += result.plainPayload
		sum.bestPayload += result.bestPayload
		sum.templatePayload += result.templatePayload
		sum.typedPayload += result.typedPayload
	}
	all := func(n int) float64 { return float64(n) / float64(sum.lines) }
	t.Logf("%-12s %5d %5s %7.1f %7.1f %7.1f %7.1f %7.1f", "all", sum.lines, "",
		all(sum.rawBytes), all(sum.plainPayload), all(sum.bestPayload), all(sum.templatePayload), all(sum.typedPayload))
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
	best, err := zstd.NewWriter(nil, zstd.WithEncoderLevel(zstd.SpeedBestCompression))
	if err != nil {
		t.Fatal(err)
	}
	var plainBlocks, templateBlocks, typedBlocks [][]byte
	blockLines := loghubBlockLines
	if size, sizeErr := strconv.Atoi(os.Getenv("TINYSTORE_LOGHUB_BLOCK")); sizeErr == nil {
		blockLines = size
	}
	for start := 0; start < len(lines); start += blockLines {
		end := min(start+blockLines, len(lines))
		plain := encoder.EncodeAll([]byte(strings.Join(lines[start:end], "\n")), nil)
		body := encodeTemplateBlock(lines[start:end], templates[start:end])
		if !slices.Equal(decodeTemplateBlock(body, miner.all), lines[start:end]) {
			t.Fatalf("%s: a line did not come back byte for byte", path)
		}
		typed := encodeTypedBlock(lines[start:end], templates[start:end])
		if !slices.Equal(decodeTypedBlock(typed, miner.all), lines[start:end]) {
			t.Fatalf("%s: a typed line did not come back byte for byte", path)
		}
		typedPacked := encoder.EncodeAll(typed, nil)
		typedBlocks = append(typedBlocks, typedPacked)
		result.typedPayload += len(typedPacked)
		result.bestPayload += len(best.EncodeAll([]byte(strings.Join(lines[start:end], "\n")), nil))
		packed := encoder.EncodeAll(body, nil)
		plainBlocks, templateBlocks = append(plainBlocks, plain), append(templateBlocks, packed)
		result.plainPayload += len(plain)
		result.templatePayload += len(packed)
	}
	dictionary := encoder.EncodeAll([]byte(templateDictionary(miner.all)), nil)
	result.templatePayload += len(dictionary)
	result.typedPayload += len(dictionary)
	result.typedFile = loghubFileSize(t, append(typedBlocks, dictionary))
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

// maxNumericDigits keeps a digit run inside int64 with room for a delta
const maxNumericDigits = 18

// tokenShape replaces each digit run with its length and returns the runs as numbers
//
//	"18:01:47,978" → "\x01\x82:\x01\x82:\x01\x82,\x01\x83" + [18 1 47 978]
func tokenShape(token string) (string, []int64) {
	if strings.ContainsAny(token, "\x00\x01") {
		return "\x00" + token, nil
	}
	var shape strings.Builder
	var numbers []int64
	for i := 0; i < len(token); {
		end := i
		for end < len(token) && token[end] >= '0' && token[end] <= '9' {
			end++
		}
		if end == i || end-i > maxNumericDigits {
			end = max(end, i+1)
			shape.WriteString(token[i:end])
			i = end
			continue
		}
		var value int64
		for _, digit := range token[i:end] {
			value = value*10 + int64(digit-'0')
		}
		shape.WriteByte(1)
		shape.WriteByte(byte(0x80 + end - i))
		numbers = append(numbers, value)
		i = end
	}
	return shape.String(), numbers
}

func shapeToken(shape string, numbers []int64) string {
	if literal, ok := strings.CutPrefix(shape, "\x00"); ok {
		return literal
	}
	var out strings.Builder
	for i := 0; i < len(shape); i++ {
		if shape[i] != 1 {
			out.WriteByte(shape[i])
			continue
		}
		width := int(shape[i+1]) - 0x80
		digits := strconv.FormatInt(numbers[0], 10)
		out.WriteString(strings.Repeat("0", width-len(digits)) + digits)
		numbers = numbers[1:]
		i++
	}
	return out.String()
}

// numberKey is a column of numbers shared by every template with the same shape at the same position,
// so a timestamp is one delta stream whichever message follows it
type numberKey struct {
	position int
	shape    string
	run      int
}

func compareNumberKeys(a, b numberKey) int {
	if a.position != b.position {
		return a.position - b.position
	}
	if a.shape != b.shape {
		return strings.Compare(a.shape, b.shape)
	}
	return a.run - b.run
}

type typedVariable struct {
	position int
	shape    string
	numbers  []int64
}

func typedVariables(line string, template *logTemplate) []typedVariable {
	var out []typedVariable
	for position, token := range lineTokens(line) {
		if template.tokens[position] == variableToken {
			shape, numbers := tokenShape(token)
			out = append(out, typedVariable{position, shape, numbers})
		}
	}
	return out
}

// encodeTypedBlock writes template ids, the shapes of every variable, then numbers as zigzag deltas per column
func encodeTypedBlock(lines []string, templates []*logTemplate) []byte {
	out := binary.AppendUvarint(nil, uint64(len(lines)))
	var shapes bytes.Buffer
	numbers := map[numberKey][]int64{}
	for index, line := range lines {
		out = binary.AppendUvarint(out, uint64(templates[index].id))
		for _, variable := range typedVariables(line, templates[index]) {
			shapes.Write(appendString(nil, variable.shape))
			for run, value := range variable.numbers {
				key := numberKey{variable.position, variable.shape, run}
				numbers[key] = append(numbers[key], value)
			}
		}
	}
	out = append(out, shapes.Bytes()...)
	keys := slices.SortedFunc(maps.Keys(numbers), compareNumberKeys)
	for _, key := range keys {
		previous := int64(0)
		for _, value := range numbers[key] {
			out = binary.AppendVarint(out, value-previous)
			previous = value
		}
	}
	return out
}

func decodeTypedBlock(body []byte, templates []*logTemplate) []string {
	count, width := binary.Uvarint(body)
	body = body[width:]
	ids := make([]int, count)
	for i := range ids {
		id, n := binary.Uvarint(body)
		ids[i], body = int(id), body[n:]
	}
	variables := make([][]typedVariable, count)
	counts := map[numberKey]int{}
	for i, id := range ids {
		for position, token := range templates[id].tokens {
			if token != variableToken {
				continue
			}
			var shape string
			shape, body = readString(body)
			runs := shapeRuns(shape)
			for run := range runs {
				counts[numberKey{position, shape, run}]++
			}
			variables[i] = append(variables[i], typedVariable{position, shape, make([]int64, runs)})
		}
	}
	columns := map[numberKey][]int64{}
	for _, key := range slices.SortedFunc(maps.Keys(counts), compareNumberKeys) {
		previous := int64(0)
		for range counts[key] {
			delta, n := binary.Varint(body)
			body = body[n:]
			previous += delta
			columns[key] = append(columns[key], previous)
		}
	}
	lines := make([]string, count)
	for i, id := range ids {
		tokens := slices.Clone(templates[id].tokens)
		for _, variable := range variables[i] {
			for run := range variable.numbers {
				key := numberKey{variable.position, variable.shape, run}
				variable.numbers[run], columns[key] = columns[key][0], columns[key][1:]
			}
			tokens[variable.position] = shapeToken(variable.shape, variable.numbers)
		}
		lines[i] = strings.Join(tokens, " ")
	}
	return lines
}

// shapeRuns counts the digit runs of a shape; an escaped literal has none
func shapeRuns(shape string) int {
	if strings.HasPrefix(shape, "\x00") {
		return 0
	}
	return strings.Count(shape, "\x01")
}

// appendString prefixes a value with its length, because real logs hold NUL bytes
func appendString(out []byte, value string) []byte {
	return append(binary.AppendUvarint(out, uint64(len(value))), value...)
}

func readString(body []byte) (string, []byte) {
	length, n := binary.Uvarint(body)
	return string(body[n : n+int(length)]), body[n+int(length):]
}
