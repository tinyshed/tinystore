// Command alibaba turns the Alibaba cluster trace's machine_usage.csv into
// influx line protocol, so the same converter that fed TSBS to three engines
// feeds this corpus too.
//
// The trace is real production data and behaves like it: timestamps are
// irregular, some columns are empty, and a machine's rows are scattered
// through the file rather than grouped. Each of those is handled here rather
// than left for an engine to disagree about.
package main

import (
	"bufio"
	"flag"
	"fmt"
	"log"
	"os"
	"sort"
	"strconv"
	"strings"
)

// machine_usage.csv, as documented in the trace's schema
var columns = []string{
	"cpu_util_percent", "mem_util_percent", "mem_gps", "mpki",
	"net_in", "net_out", "disk_io_percent",
}

// a machine's rows are scattered through the file rather than grouped, so
// every selected row is held until the end. The seven fields stay as the one
// substring they arrived in: seven separate strings would cost more in slice
// headers than in data.
type row struct {
	at   int64
	rest string
}

func main() {
	input := flag.String("in", "", "machine_usage.csv")
	out := flag.String("out", "", "influx line protocol")
	modulus := flag.Int("every", 16, "keep machines whose numeric id is a multiple of this")
	until := flag.Int64("until", 172800, "keep samples strictly before this trace second, 0 for all")
	base := flag.Int64("base", 1767225600000, "milliseconds the trace's second zero maps to")
	flag.Parse()
	if *input == "" || *out == "" {
		log.Fatal("need -in and -out")
	}

	source, err := os.Open(*input)
	if err != nil {
		log.Fatal(err)
	}
	defer source.Close()
	target, err := os.Create(*out)
	if err != nil {
		log.Fatal(err)
	}
	writer := bufio.NewWriterSize(target, 1<<20)

	scanner := bufio.NewScanner(source)
	scanner.Buffer(make([]byte, 0, 1<<16), 1<<20)
	byMachine := map[string][]row{}
	var read, selected int

	for scanner.Scan() {
		line := scanner.Text()
		if line == "" {
			continue
		}
		read++
		first := strings.IndexByte(line, ',')
		if first < 0 {
			log.Fatalf("no machine id: %.80s", line)
		}
		second := strings.IndexByte(line[first+1:], ',')
		if second < 0 {
			log.Fatalf("no timestamp: %.80s", line)
		}
		second += first + 1
		id, err := strconv.Atoi(strings.TrimPrefix(line[:first], "m_"))
		if err != nil {
			log.Fatalf("machine id: %v", err)
		}
		if id%*modulus != 0 {
			continue
		}
		at, err := strconv.ParseInt(line[first+1:second], 10, 64)
		if err != nil {
			log.Fatalf("timestamp: %v", err)
		}
		if *until > 0 && at >= *until {
			continue
		}
		machine := line[:first]
		selected++
		byMachine[strings.Clone(machine)] = append(byMachine[machine],
			row{at: at, rest: strings.Clone(line[second+1:])})
	}
	if err := scanner.Err(); err != nil {
		log.Fatal(err)
	}

	names := make([]string, 0, len(byMachine))
	for machine := range byMachine {
		names = append(names, machine)
	}
	sort.Strings(names)

	var samples, duplicates, dropped int
	for _, machine := range names {
		rows := byMachine[machine]
		sort.SliceStable(rows, func(i, j int) bool { return rows[i].at < rows[j].at })
		kept := rows[:0]
		for _, r := range rows {
			if len(kept) > 0 && r.at == kept[len(kept)-1].at {
				duplicates++
				kept[len(kept)-1] = r // last row for a timestamp wins, as in the NAB corpus
				continue
			}
			kept = append(kept, r)
		}
		for _, r := range kept {
			values := strings.Split(r.rest, ",")
			if len(values) != len(columns) {
				log.Fatalf("expected %d value columns, got %d: %.80s", len(columns), len(values), r.rest)
			}
			var fields []string
			for i, text := range values {
				text = strings.TrimSpace(text)
				if text == "" {
					continue
				}
				// the text must survive float64, or an engine would be asked to
				// store something this corpus cannot claim to have given it
				if _, err := strconv.ParseFloat(text, 64); err != nil {
					log.Fatalf("value %q: %v", text, err)
				}
				fields = append(fields, columns[i]+"="+text)
			}
			if len(fields) == 0 {
				dropped++
				continue
			}
			samples += len(fields)
			fmt.Fprintf(writer, "machine,machine_id=%s %s %d000000\n",
				machine, strings.Join(fields, ","), *base+r.at*1000)
		}
	}
	if err := writer.Flush(); err != nil {
		log.Fatal(err)
	}
	if err := target.Close(); err != nil {
		log.Fatal(err)
	}
	fmt.Printf("rows-read=%d rows-selected=%d machines=%d samples=%d duplicate-timestamps=%d empty-rows=%d\n",
		read, selected, len(names), samples, duplicates, dropped)
}
