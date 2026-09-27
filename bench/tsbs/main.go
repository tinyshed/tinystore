// Command tsbs turns one TSBS line-protocol file into the three inputs the
// comparison needs: a JSONL corpus that is also VictoriaMetrics' import format,
// an OpenMetrics file for promtool's backfill, and the line protocol itself.
//
// Every engine must receive the same samples, so the conversion happens once
// and the three outputs are written from one parse.
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
	name       string
	labels     []string // alternating key, value
	timestamps []int64
	values     []float64
}

func main() {
	input := flag.String("in", "", "TSBS influx line protocol file")
	corpus := flag.String("corpus", "", "JSONL corpus, also the VictoriaMetrics import body")
	openmetrics := flag.String("openmetrics", "", "OpenMetrics file for promtool backfill")
	flag.Parse()
	if *input == "" || *corpus == "" {
		log.Fatal("need -in and -corpus")
	}

	f, err := os.Open(*input)
	if err != nil {
		log.Fatal(err)
	}
	defer f.Close()

	order := []string{}
	byKey := map[string]*series{}
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 0, 1<<20), 1<<24)
	var integers, floats int
	for scanner.Scan() {
		line := scanner.Text()
		if line == "" {
			continue
		}
		head, rest, ok := strings.Cut(line, " ")
		if !ok {
			log.Fatalf("no field set: %.80s", line)
		}
		fields, stamp, ok := strings.Cut(rest, " ")
		if !ok {
			log.Fatalf("no timestamp: %.80s", line)
		}
		nanos, err := strconv.ParseInt(stamp, 10, 64)
		if err != nil {
			log.Fatal(err)
		}
		millis := nanos / 1e6
		if millis*1e6 != nanos {
			log.Fatalf("timestamp is not a whole millisecond: %d", nanos)
		}
		parts := strings.Split(head, ",")
		measurement, tags := parts[0], parts[1:]
		for field := range strings.SplitSeq(fields, ",") {
			key, raw, ok := strings.Cut(field, "=")
			if !ok {
				log.Fatalf("no field value: %s", field)
			}
			var value float64
			if before, ok0 := strings.CutSuffix(raw, "i"); ok0 {
				n, err := strconv.ParseInt(before, 10, 64)
				if err != nil {
					log.Fatal(err)
				}
				// an integer past 2^53 would not survive float64, and every
				// engine here stores float64, so the corpus must not contain one
				if n > 1<<53 || n < -(1<<53) {
					log.Fatalf("%s=%d does not fit a float64 exactly", key, n)
				}
				value, integers = float64(n), integers+1
			} else {
				value, err = strconv.ParseFloat(raw, 64)
				if err != nil {
					log.Fatal(err)
				}
				floats++
			}
			name := measurement + "_" + key
			identity := name + "\x00" + strings.Join(tags, "\x00")
			s := byKey[identity]
			if s == nil {
				s = &series{name: name}
				for _, tag := range tags {
					k, v, ok := strings.Cut(tag, "=")
					if !ok {
						log.Fatalf("no tag value: %s", tag)
					}
					s.labels = append(s.labels, k, v)
				}
				byKey[identity] = s
				order = append(order, identity)
			}
			s.timestamps = append(s.timestamps, millis)
			s.values = append(s.values, value)
		}
	}
	if err := scanner.Err(); err != nil {
		log.Fatal(err)
	}
	sort.Strings(order)

	out, err := os.Create(*corpus)
	if err != nil {
		log.Fatal(err)
	}
	corpusWriter := bufio.NewWriterSize(out, 1<<20)
	var samples int
	for _, identity := range order {
		s := byKey[identity]
		samples += len(s.values)
		fmt.Fprintf(corpusWriter, `{"metric":{"__name__":%s`, quote(s.name))
		for i := 0; i < len(s.labels); i += 2 {
			fmt.Fprintf(corpusWriter, `,%s:%s`, quote(s.labels[i]), quote(s.labels[i+1]))
		}
		corpusWriter.WriteString(`},"values":[`)
		for i, v := range s.values {
			if i > 0 {
				corpusWriter.WriteByte(',')
			}
			// the shortest round-tripping form, so the JSON carries every bit
			corpusWriter.WriteString(strconv.FormatFloat(v, 'g', -1, 64))
		}
		corpusWriter.WriteString(`],"timestamps":[`)
		for i, ts := range s.timestamps {
			if i > 0 {
				corpusWriter.WriteByte(',')
			}
			corpusWriter.WriteString(strconv.FormatInt(ts, 10))
		}
		corpusWriter.WriteString("]}\n")
	}
	if err := corpusWriter.Flush(); err != nil {
		log.Fatal(err)
	}
	if err := out.Close(); err != nil {
		log.Fatal(err)
	}

	if *openmetrics != "" {
		om, err := os.Create(*openmetrics)
		if err != nil {
			log.Fatal(err)
		}
		w := bufio.NewWriterSize(om, 1<<20)
		declared := map[string]bool{}
		for _, identity := range order {
			s := byKey[identity]
			if !declared[s.name] {
				fmt.Fprintf(w, "# TYPE %s gauge\n", s.name)
				declared[s.name] = true
			}
			for i := range s.values {
				w.WriteString(s.name)
				w.WriteByte('{')
				for k := 0; k < len(s.labels); k += 2 {
					if k > 0 {
						w.WriteByte(',')
					}
					fmt.Fprintf(w, `%s=%s`, s.labels[k], quote(s.labels[k+1]))
				}
				fmt.Fprintf(w, "} %s %s\n",
					strconv.FormatFloat(s.values[i], 'g', -1, 64),
					strconv.FormatFloat(float64(s.timestamps[i])/1000, 'f', -1, 64))
			}
		}
		w.WriteString("# EOF\n")
		if err := w.Flush(); err != nil {
			log.Fatal(err)
		}
		if err := om.Close(); err != nil {
			log.Fatal(err)
		}
	}

	exact := 0
	for _, identity := range order {
		for _, v := range byKey[identity].values {
			parsed, err := strconv.ParseFloat(strconv.FormatFloat(v, 'g', -1, 64), 64)
			if err != nil || math.Float64bits(parsed) != math.Float64bits(v) {
				log.Fatalf("value %v does not survive its own text form", v)
			}
			exact++
		}
	}
	fmt.Printf("series=%d samples=%d integers=%d floats=%d text-round-trip-checked=%d\n",
		len(order), samples, integers, floats, exact)
}

func quote(s string) string {
	return strconv.Quote(s)
}
