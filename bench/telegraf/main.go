// Command telegraf turns the line protocol a real telegraf agent wrote into the
// same normalized JSONL corpus the other runners produce, so one harness reads
// every corpus and the numbers are comparable.
//
// It is deliberately tolerant where the TSBS converter is strict: a real agent
// emits strings and booleans, series appear and disappear with the containers
// they describe, and two inputs may run at different intervals.
package main

import (
	"bufio"
	"flag"
	"fmt"
	"log"
	"math"
	"os"
	"sort"
	"strconv"
	"strings"
)

type series struct {
	name   string
	labels []string // alternating key, value
	points []point
}

type point struct {
	at    int64
	value float64
}

// splitUnescaped cuts on the first separator that is neither escaped nor inside
// a quoted field value, which a naive Split cannot do on real agent output
func splitUnescaped(s string, sep byte) []string {
	var out []string
	var start int
	quoted, escaped := false, false
	for i := 0; i < len(s); i++ {
		switch {
		case escaped:
			escaped = false
		case s[i] == '\\':
			escaped = true
		case s[i] == '"':
			quoted = !quoted
		case s[i] == sep && !quoted:
			out = append(out, s[start:i])
			start = i + 1
		}
	}
	return append(out, s[start:])
}

func unescape(s string) string {
	if !strings.ContainsRune(s, '\\') {
		return s
	}
	var b strings.Builder
	for i := 0; i < len(s); i++ {
		if s[i] == '\\' && i+1 < len(s) {
			i++
		}
		b.WriteByte(s[i])
	}
	return b.String()
}

// numberOf returns the field's value, or false for the strings and booleans a
// real agent also writes
func numberOf(raw string) (float64, bool, bool) {
	if raw == "" {
		return 0, false, false
	}
	switch raw[len(raw)-1] {
	case 'i', 'u':
		n, err := strconv.ParseInt(strings.TrimSuffix(strings.TrimSuffix(raw, "i"), "u"), 10, 64)
		if err != nil {
			u, uerr := strconv.ParseUint(strings.TrimSuffix(raw, "u"), 10, 64)
			if uerr != nil {
				return 0, false, false
			}
			if u > 1<<53 {
				return 0, false, false
			}
			return float64(u), true, true
		}
		if n > 1<<53 || n < -(1<<53) {
			return 0, false, false
		}
		return float64(n), true, true
	}
	if raw[0] == '"' || raw == "t" || raw == "f" || raw == "true" || raw == "false" ||
		raw == "T" || raw == "F" || raw == "TRUE" || raw == "FALSE" {
		return 0, false, false
	}
	v, err := strconv.ParseFloat(raw, 64)
	if err != nil {
		return 0, false, false
	}
	return v, false, true
}

func main() {
	corpus := flag.String("corpus", "", "JSONL corpus to write")
	minSamples := flag.Int("min-samples", 2, "drop series with fewer samples than this")
	drop := flag.String("drop", "", "comma separated measurement prefixes to leave out of the corpus")
	maxLabels := flag.Int("max-labels", 0, "leave out series carrying more labels than this, zero for no bound")
	flag.Parse()
	if *corpus == "" || flag.NArg() == 0 {
		log.Fatal("need -corpus and at least one line protocol file")
	}

	var dropped []string
	if *drop != "" {
		dropped = strings.Split(*drop, ",")
	}
	order := []string{}
	byKey := map[string]*series{}
	excluded := 0
	var integers, floats, skipped, lines int
	for _, path := range flag.Args() {
		f, err := os.Open(path)
		if err != nil {
			log.Fatal(err)
		}
		scanner := bufio.NewScanner(f)
		scanner.Buffer(make([]byte, 0, 1<<20), 1<<24)
		for scanner.Scan() {
			line := scanner.Text()
			if line == "" || line[0] == '#' {
				continue
			}
			lines++
			parts := splitUnescaped(line, ' ')
			if len(parts) < 3 {
				continue
			}
			head := parts[0]
			stamp := parts[len(parts)-1]
			fields := strings.Join(parts[1:len(parts)-1], " ")
			nanos, err := strconv.ParseInt(stamp, 10, 64)
			if err != nil {
				continue
			}
			millis := nanos / 1e6
			tagParts := splitUnescaped(head, ',')
			measurement := unescape(tagParts[0])
			skip := false
			for _, prefix := range dropped {
				skip = skip || strings.HasPrefix(measurement, prefix)
			}
			if skip {
				excluded++
				continue
			}
			for _, field := range splitUnescaped(fields, ',') {
				key, raw, ok := strings.Cut(field, "=")
				if !ok {
					continue
				}
				value, isInteger, numeric := numberOf(raw)
				if !numeric {
					skipped++
					continue
				}
				if isInteger {
					integers++
				} else {
					floats++
				}
				name := measurement + "_" + unescape(key)
				identity := name + "\x00" + strings.Join(tagParts[1:], "\x00")
				s := byKey[identity]
				if s == nil {
					s = &series{name: name}
					for _, tag := range tagParts[1:] {
						k, v, ok := strings.Cut(tag, "=")
						if !ok {
							continue
						}
						s.labels = append(s.labels, unescape(k), unescape(v))
					}
					byKey[identity] = s
					order = append(order, identity)
				}
				s.points = append(s.points, point{at: millis, value: value})
			}
		}
		if err := scanner.Err(); err != nil {
			log.Fatal(err)
		}
		f.Close()
	}
	sort.Strings(order)

	out, err := os.Create(*corpus)
	if err != nil {
		log.Fatal(err)
	}
	w := bufio.NewWriterSize(out, 1<<20)
	var samples, written, duplicates, short, wide int
	for _, identity := range order {
		s := byKey[identity]
		sort.SliceStable(s.points, func(i, j int) bool { return s.points[i].at < s.points[j].at })
		kept := s.points[:0]
		for _, p := range s.points {
			if len(kept) > 0 && kept[len(kept)-1].at == p.at {
				kept[len(kept)-1] = p
				duplicates++
				continue
			}
			kept = append(kept, p)
		}
		s.points = kept
		if len(s.points) < *minSamples {
			short++
			continue
		}
		if *maxLabels > 0 && len(s.labels)/2+1 > *maxLabels {
			wide++
			continue
		}
		samples += len(s.points)
		written++
		fmt.Fprintf(w, `{"metric":{"__name__":%s`, quote(s.name))
		for i := 0; i < len(s.labels); i += 2 {
			fmt.Fprintf(w, `,%s:%s`, quote(s.labels[i]), quote(s.labels[i+1]))
		}
		w.WriteString(`},"values":[`)
		for i, p := range s.points {
			if i > 0 {
				w.WriteByte(',')
			}
			w.WriteString(strconv.FormatFloat(p.value, 'g', -1, 64))
		}
		w.WriteString(`],"timestamps":[`)
		for i, p := range s.points {
			if i > 0 {
				w.WriteByte(',')
			}
			w.WriteString(strconv.FormatInt(p.at, 10))
		}
		w.WriteString("]}\n")
	}
	if err := w.Flush(); err != nil {
		log.Fatal(err)
	}
	if err := out.Close(); err != nil {
		log.Fatal(err)
	}

	// the corpus travels as text, so every value must survive its own text form
	for _, identity := range order {
		for _, p := range byKey[identity].points {
			parsed, err := strconv.ParseFloat(strconv.FormatFloat(p.value, 'g', -1, 64), 64)
			if err != nil || math.Float64bits(parsed) != math.Float64bits(p.value) {
				log.Fatalf("value %v does not survive its own text form", p.value)
			}
		}
	}
	fmt.Printf("lines=%d series=%d written=%d short=%d samples=%d integers=%d floats=%d non-numeric=%d duplicates=%d excluded-lines=%d too-many-labels=%d\n",
		lines, len(order), written, short, samples, integers, floats, skipped, duplicates, excluded, wide)
}

func quote(s string) string {
	out, err := jsonString(s)
	if err != nil {
		log.Fatal(err)
	}
	return out
}

func jsonString(s string) (string, error) {
	var b strings.Builder
	b.WriteByte('"')
	for _, r := range s {
		switch r {
		case '"':
			b.WriteString(`\"`)
		case '\\':
			b.WriteString(`\\`)
		case '\n':
			b.WriteString(`\n`)
		case '\r':
			b.WriteString(`\r`)
		case '\t':
			b.WriteString(`\t`)
		default:
			if r < 0x20 {
				fmt.Fprintf(&b, `\u%04x`, r)
				continue
			}
			b.WriteRune(r)
		}
	}
	b.WriteByte('"')
	return b.String(), nil
}
