// Command sqlshape compares bounded prepared-cache churn with one JSON-list shape.
package main

import (
	"bufio"
	"context"
	"database/sql"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"os"
	"runtime"
	"slices"
	"strings"
	"time"

	_ "modernc.org/sqlite"
)

const jsonLookup = `select lv.id from json_each(?) j join label_values lv on lv.name=json_extract(j.value,'$[0]') and lv.value=json_extract(j.value,'$[1]')`

type statementCache struct {
	conn                *sql.Conn
	statements          map[string]*sql.Stmt
	order               []string
	prepares, evictions int
}

func (c *statementCache) get(ctx context.Context, query string) (*sql.Stmt, error) {
	if statement := c.statements[query]; statement != nil {
		for i, key := range c.order {
			if key == query {
				c.order = append(c.order[:i], c.order[i+1:]...)
				break
			}
		}
		c.order = append(c.order, query)
		return statement, nil
	}
	if len(c.order) == 32 {
		oldest := c.order[0]
		if err := c.statements[oldest].Close(); err != nil {
			return nil, err
		}
		delete(c.statements, oldest)
		c.order = c.order[1:]
		c.evictions++
	}
	statement, err := c.conn.PrepareContext(ctx, query)
	if err != nil {
		return nil, err
	}
	c.statements[query] = statement
	c.order = append(c.order, query)
	c.prepares++
	return statement, nil
}

func (c *statementCache) close() {
	for _, statement := range c.statements {
		if err := statement.Close(); err != nil {
			log.Fatal(err)
		}
	}
}

func loadLabels(path string) [][][2]string {
	file, err := os.Open(path)
	if err != nil {
		log.Fatal(err)
	}
	defer file.Close()
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 1<<20), 1<<28)
	var requests [][][2]string
	for scanner.Scan() {
		var row struct {
			Metric map[string]string `json:"metric"`
		}
		if err := json.Unmarshal(scanner.Bytes(), &row); err != nil {
			log.Fatal(err)
		}
		names := make([]string, 0, len(row.Metric))
		for name := range row.Metric {
			names = append(names, name)
		}
		slices.Sort(names)
		pairs := make([][2]string, 0, len(names))
		for _, name := range names {
			pairs = append(pairs, [2]string{name, row.Metric[name]})
		}
		requests = append(requests, pairs)
	}
	if err := scanner.Err(); err != nil {
		log.Fatal(err)
	}
	return requests
}

func syntheticLabels(db *sql.DB, shapes int) [][][2]string {
	rows, err := db.Query(`select name,min(value) from label_values group by name order by name`)
	if err != nil {
		log.Fatal(err)
	}
	defer rows.Close()
	var available [][2]string
	for rows.Next() {
		var pair [2]string
		if err := rows.Scan(&pair[0], &pair[1]); err != nil {
			log.Fatal(err)
		}
		available = append(available, pair)
	}
	if err := rows.Err(); err != nil {
		log.Fatal(err)
	}
	if shapes > len(available) {
		log.Fatalf("need %d distinct label names, found %d", shapes, len(available))
	}
	requests := make([][][2]string, shapes)
	for count := range requests {
		requests[count] = available[:count+1]
	}
	return requests
}

func lookup(ctx context.Context, cache *statementCache, pairs [][2]string, mode string) error {
	var query string
	var arguments []any
	if mode == "dynamic" {
		query = `select id from label_values where (name,value) in (values ` + strings.Repeat("(?,?),", len(pairs)-1) + `(?,?))`
		arguments = make([]any, 0, len(pairs)*2)
		for _, pair := range pairs {
			arguments = append(arguments, pair[0], pair[1])
		}
	} else {
		query = jsonLookup
		encoded, err := json.Marshal(pairs)
		if err != nil {
			return err
		}
		arguments = []any{string(encoded)}
	}
	statement, err := cache.get(ctx, query)
	if err != nil {
		return err
	}
	rows, err := statement.QueryContext(ctx, arguments...)
	if err != nil {
		return err
	}
	count := 0
	for rows.Next() {
		var id int64
		if err := rows.Scan(&id); err != nil {
			rows.Close()
			return err
		}
		count++
	}
	rowErr := rows.Err()
	if err := rows.Close(); err != nil {
		return err
	}
	if rowErr != nil {
		return rowErr
	}
	if count != len(pairs) {
		return fmt.Errorf("lookup returned %d of %d label pairs", count, len(pairs))
	}
	return nil
}

func measure(ctx context.Context, db *sql.DB, requests [][][2]string, mode string, iterations int) {
	conn, err := db.Conn(ctx)
	if err != nil {
		log.Fatal(err)
	}
	defer conn.Close()
	cache := &statementCache{conn: conn, statements: map[string]*sql.Stmt{}}
	defer cache.close()
	for i := range min(iterations, len(requests)*2) {
		if err := lookup(ctx, cache, requests[(i*7919)%len(requests)], mode); err != nil {
			log.Fatal(err)
		}
	}
	prepares, evictions := cache.prepares, cache.evictions
	var before, after runtime.MemStats
	runtime.GC()
	runtime.ReadMemStats(&before)
	start := time.Now()
	for i := range iterations {
		if err := lookup(ctx, cache, requests[(i*7919)%len(requests)], mode); err != nil {
			log.Fatal(err)
		}
	}
	elapsed := time.Since(start)
	runtime.ReadMemStats(&after)
	fmt.Printf("mode=%s operations=%d ns_per_lookup=%.1f allocs_per_lookup=%.1f prepares=%d evictions=%d\n", mode, iterations,
		float64(elapsed.Nanoseconds())/float64(iterations), float64(after.Mallocs-before.Mallocs)/float64(iterations),
		cache.prepares-prepares, cache.evictions-evictions)
}

func main() {
	path := flag.String("db", "", "prepared TinyStore database")
	corpus := flag.String("corpus", "", "matching normalized JSONL")
	iterations := flag.Int("operations", 20000, "lookups per stage")
	shapes := flag.Int("synthetic-shapes", 0, "use 1..N distinct-name label counts")
	flag.Parse()
	if *path == "" || *iterations < 1 || *shapes < 0 || *shapes == 0 && *corpus == "" {
		log.Fatal("database, corpus or synthetic shapes, and positive operation count required")
	}
	db, err := sql.Open("sqlite", *path)
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()
	db.SetMaxOpenConns(1)
	var requests [][][2]string
	if *shapes > 0 {
		requests = syntheticLabels(db, *shapes)
	} else {
		requests = loadLabels(*corpus)
	}
	if len(requests) == 0 {
		log.Fatal("empty request set")
	}
	ctx := context.Background()
	for _, mode := range []string{"dynamic", "json", "json", "dynamic"} {
		measure(ctx, db, requests, mode, *iterations)
	}
}
