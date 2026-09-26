package records

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/klauspost/compress/zstd"
)

// measurements on a private docker json-file corpus, as bench/fetch-docker-logs.sh
// leaves it; skipped unless TINYSTORE_SPIKE=1 and TINYSTORE_CORPUS name it

// corpusTexts are the bodies of the text records a writer of Lines makes of
// each container's entries, at the times docker received them, and those times
func corpusTexts(t *testing.T) (bodies [][]string, times [][]int64) {
	t.Helper()
	root := os.Getenv("TINYSTORE_CORPUS")
	if os.Getenv("TINYSTORE_SPIKE") != "1" || root == "" {
		t.Skip("a corpus measurement: set TINYSTORE_SPIKE=1 and TINYSTORE_CORPUS")
	}
	hosts, err := os.ReadDir(root)
	if err != nil {
		t.Fatal(err)
	}
	for _, host := range hosts {
		containers, _ := filepath.Glob(filepath.Join(root, host.Name(), "raw", "*"))
		slices.Sort(containers)
		for _, dir := range containers {
			texts, at := containerTexts(t, dir)
			bodies, times = append(bodies, texts), append(times, at)
		}
	}
	return bodies, times
}

func containerTexts(t *testing.T, dir string) ([]string, []int64) {
	var bodies []string
	var times []int64
	var received time.Time
	w := newLineWriter("corpus", func() time.Time { return received }, func(r Record) {
		if r.Name == textLine {
			bodies, times = append(bodies, *r.Body), append(times, r.At.UnixNano())
		}
	})
	for _, entry := range dockerEntries(t, dir) {
		received = entry.at
		if _, err := w.Write([]byte(entry.text)); err != nil {
			t.Fatal(err)
		}
	}
	if err := w.Close(); err != nil {
		t.Fatal(err)
	}
	return bodies, times
}

type dockerEntry struct {
	at   time.Time
	text string
}

// dockerEntries reads a container's log files, oldest rotation first
func dockerEntries(t *testing.T, dir string) []dockerEntry {
	files, _ := filepath.Glob(filepath.Join(dir, "*-json.log*"))
	rotation := func(path string) int {
		if at := strings.LastIndex(path, ".log."); at >= 0 {
			n, _ := strconv.Atoi(path[at+5:])
			return n
		}
		return 0
	}
	slices.SortFunc(files, func(a, b string) int { return rotation(b) - rotation(a) })
	var entries []dockerEntry
	for _, path := range files {
		file, err := os.Open(path)
		if err != nil {
			t.Fatal(err)
		}
		scanner := bufio.NewScanner(file)
		scanner.Buffer(make([]byte, 64<<10), 16<<20)
		for scanner.Scan() {
			var entry struct{ Log, Time string }
			if json.Unmarshal(scanner.Bytes(), &entry) != nil {
				continue
			}
			if at, err := time.Parse(time.RFC3339Nano, entry.Time); err == nil {
				entries = append(entries, dockerEntry{at: at, text: entry.Log})
			}
		}
		_ = file.Close()
	}
	return entries
}

// the text of the production corpus: what the body column costs as the engine
// writes it, what is left of it past the stamps a block at a time at three
// zstd levels and as one frame a segment, and templated with the numbers
// standing in it typed, in one column a block and in a column for each place
// in each template
func TestTextTemplatesAgainstZstd(t *testing.T) {
	bodies, times := corpusTexts(t)
	e, _ := testCoders(t)
	levels := map[string]*zstd.Encoder{}
	for name, level := range map[string]zstd.EncoderLevel{
		"better": zstd.SpeedBetterCompression, "best": zstd.SpeedBestCompression, "default": zstd.SpeedDefault,
	} {
		encoder, err := newBlobEncoder(level)
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { _ = encoder.Close() })
		levels[name] = encoder
	}
	var cost textCost
	for i := range bodies {
		for start := 0; start < len(bodies[i]); start += maxSegmentRecords {
			end := min(start+maxSegmentRecords, len(bodies[i]))
			cost.add(measureTexts(e, levels, bodies[i][start:end], times[i][start:end]))
		}
	}
	per := func(n int) string { return fmt.Sprintf("%.3f", float64(n)/float64(cost.records)) }
	fmt.Printf("text_records=%d column=%s stamps=%s rest_default=%s rest_better=%s rest_best=%s "+
		"rest_segment_frame=%s templated_one_column=%s templated_by_place=%s template_dictionary=%s\n",
		cost.records, per(cost.column), per(cost.stamps), per(cost.rest["default"]), per(cost.rest["better"]),
		per(cost.rest["best"]), per(cost.segmentFrame), per(cost.templated+cost.dictionary),
		per(cost.byPlace+cost.dictionary), per(cost.dictionary))
	fmt.Printf("zstd_time default=%v better=%v best=%v\n", cost.took["default"], cost.took["better"], cost.took["best"])
}

type textCost struct {
	records, column, stamps, segmentFrame, templated, byPlace, dictionary int
	rest                                                                  map[string]int
	took                                                                  map[string]time.Duration
}

func (c *textCost) add(o textCost) {
	c.records += o.records
	c.column += o.column
	c.stamps += o.stamps
	c.segmentFrame += o.segmentFrame
	c.templated += o.templated
	c.byPlace += o.byPlace
	c.dictionary += o.dictionary
	if c.rest == nil {
		c.rest, c.took = map[string]int{}, map[string]time.Duration{}
	}
	for level, size := range o.rest {
		c.rest[level] += size
		c.took[level] += o.took[level]
	}
}

// measureTexts is one segment's bodies, a block at a time
func measureTexts(e *encoder, levels map[string]*zstd.Encoder, bodies []string, times []int64) textCost {
	cost := textCost{records: len(bodies), rest: map[string]int{}, took: map[string]time.Duration{}}
	var rests []string
	for start := 0; start < len(bodies); start += maxBlockRecords {
		end := min(start+maxBlockRecords, len(bodies))
		cost.column += len(e.appendValues(nil, bodies[start:end], times[start:end]))
		rest := bodies[start:end]
		if e.findColumnStamps(rest, times[start:end]) {
			column := e.cutStamps(rest, times[start:end])
			for _, ints := range [][]int64{column.counts, column.layoutIDs, column.gaps, column.behind} {
				cost.stamps += len(e.appendInts(nil, ints))
			}
			rest = column.rest
		}
		shared := e.zstd
		for level, encoder := range levels {
			e.zstd = encoder
			started := time.Now()
			cost.rest[level] += len(e.appendTexts(nil, rest))
			cost.took[level] += time.Since(started)
		}
		e.zstd = shared
		rests = append(rests, rest...)
	}
	cost.segmentFrame = len(e.zstd.EncodeAll([]byte(strings.Join(rests, "\n")), nil))
	cost.templated, cost.byPlace, cost.dictionary = templatedCost(e, rests)
	return cost
}

// templateCut is a text cut into its template, the numbers standing in it and
// the hex words: a number is a digit run a letter or digit does not precede,
// canonical and at most eighteen digits long, and a hex word eight hex digits
// or more with both a digit and a letter
type templateCut struct {
	template string
	numbers  []int64
	words    []string
}

func cutTemplate(text string) (templateCut, bool) {
	if strings.ContainsAny(text, "\x00\x01") {
		return templateCut{}, false
	}
	var cut templateCut
	var b strings.Builder
	for i := 0; i < len(text); {
		if width := hexWord(text, i); width > 0 {
			cut.words = append(cut.words, text[i:i+width])
			b.WriteByte(1)
			i += width
			continue
		}
		width := numberAt(text, i)
		if number, ok := parseInt(text[i : i+width]); ok && width > 0 && width <= 18 {
			cut.numbers = append(cut.numbers, number)
			b.WriteByte(0)
			i += width
			continue
		}
		b.WriteString(text[i : i+max(1, width)])
		i += max(1, width)
	}
	cut.template = b.String()
	return cut, true
}

func numberAt(text string, i int) int {
	if i > 0 && (isLetter(text[i-1]) || isDigit(text[i-1])) {
		return 0
	}
	end := i
	for end < len(text) && isDigit(text[end]) {
		end++
	}
	return end - i
}

func hexWord(text string, i int) int {
	if i > 0 && (isLetter(text[i-1]) || isDigit(text[i-1])) {
		return 0
	}
	end, digits, letters := i, 0, 0
	for ; end < len(text); end++ {
		c := text[end]
		if isDigit(c) {
			digits++
		} else if c >= 'a' && c <= 'f' {
			letters++
		} else {
			break
		}
	}
	if end-i < 8 || digits == 0 || letters == 0 || (end < len(text) && isLetter(text[end])) {
		return 0
	}
	return end - i
}

// templatedCost is what rests cost with a template dictionary a segment, the
// templates seen twice or more: a block writes each value's template id, its
// numbers and hex words, and the values of no template as text
func templatedCost(e *encoder, rests []string) (oneColumn, byPlace, dictionary int) {
	cuts := make([]templateCut, len(rests))
	cut := make([]bool, len(rests))
	counts := map[string]int{}
	for i, rest := range rests {
		if cuts[i], cut[i] = cutTemplate(rest); cut[i] {
			counts[cuts[i].template]++
		}
	}
	var templates []string
	for template, n := range counts {
		if n >= 2 {
			templates = append(templates, template)
		}
	}
	slices.Sort(templates)
	ids := map[string]int64{}
	for i, template := range templates {
		ids[template] = int64(i + 1)
	}
	dictionary = len(e.zstd.EncodeAll([]byte(strings.Join(templates, "\n")), nil))
	for start := 0; start < len(rests); start += maxBlockRecords {
		end := min(start+maxBlockRecords, len(rests))
		var templateIDs, numbers []int64
		var words, texts []string
		byPlaceNumbers := map[[2]int64][]int64{}
		var places [][2]int64
		for i := start; i < end; i++ {
			id := ids[cuts[i].template]
			if !cut[i] || id == 0 {
				templateIDs, texts = append(templateIDs, 0), append(texts, rests[i])
				continue
			}
			templateIDs, numbers = append(templateIDs, id), append(numbers, cuts[i].numbers...)
			words = append(words, cuts[i].words...)
			for place, number := range cuts[i].numbers {
				key := [2]int64{id, int64(place)}
				if _, ok := byPlaceNumbers[key]; !ok {
					places = append(places, key)
				}
				byPlaceNumbers[key] = append(byPlaceNumbers[key], number)
			}
		}
		shared := len(e.appendInts(nil, templateIDs)) + len(e.appendTexts(nil, words)) + len(e.appendTexts(nil, texts))
		oneColumn += shared + len(e.appendInts(nil, numbers))
		byPlace += shared
		for _, key := range places {
			byPlace += len(e.appendInts(nil, byPlaceNumbers[key]))
		}
	}
	return oneColumn, byPlace, dictionary
}
