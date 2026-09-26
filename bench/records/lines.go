package main

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"log/slog"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/tinyshed/tinystore/records"
)

// measureLines hands every container's output to a writer of Lines, as a
// program following those containers would: each docker entry at the time
// docker received it, a flush every few hundred entries, full segments sealed
// as they fill. It reports what the writer joined and found, what the file
// costs, and what a read for errors fetches through the blocks' level masks.
func measureLines(ctx context.Context, dir, root string) {
	h := openHarness(ctx, dir, time.Unix(0, 0))
	entries, start := 0, time.Now()
	for _, found := range containerLogs(root) {
		entries += h.follow(ctx, found)
	}
	h.flush(ctx)
	h.sealAll(ctx, h.clock())
	stats := h.logs.Stats()
	fmt.Printf("stage=lines entries=%d records=%d dropped=%d append_and_seal=%v\n", entries, stats.Appended,
		stats.Dropped, time.Since(start).Round(time.Millisecond))
	errors := linesCensus(ctx, h)
	askLevel(ctx, h, errors)
	h.close(ctx)
	reportFile(ctx, dir, counted(stats.Appended))
}

// counted is a count Stats gives as an int
func counted(n uint64) int {
	return int(n) //nolint:gosec // a corpus's records, far below an int's range
}

// containerLog is one container's log files, oldest rotation first
type containerLog struct {
	name  string
	files []string
}

func containerLogs(root string) []containerLog {
	hosts, err := os.ReadDir(root)
	if err != nil {
		log.Fatal(err)
	}
	var logs []containerLog
	for _, host := range hosts {
		for id, name := range containerNames(filepath.Join(root, host.Name())) {
			logs = append(logs, containerLog{
				name:  host.Name() + "/" + name,
				files: logFiles(filepath.Join(root, host.Name(), "raw", id)),
			})
		}
	}
	slices.SortFunc(logs, func(a, b containerLog) int { return strings.Compare(a.name, b.name) })
	return logs
}

// follow writes one container's entries to a writer of Lines of its own,
// moving the store's clock to each entry's time, and returns how many it wrote
func (h *harness) follow(ctx context.Context, found containerLog) int {
	w := h.logs.Lines(found.name)
	written := 0
	for _, path := range found.files {
		file, err := os.Open(path) //nolint:gosec // a log file of the corpus the command was given
		if err != nil {
			log.Fatal(err)
		}
		scanner := bufio.NewScanner(file)
		scanner.Buffer(make([]byte, 64<<10), 16<<20)
		for scanner.Scan() {
			var entry struct{ Log, Time string }
			if json.Unmarshal(scanner.Bytes(), &entry) != nil {
				continue
			}
			at, parseErr := time.Parse(time.RFC3339Nano, entry.Time)
			if parseErr != nil {
				continue
			}
			h.setClock(at.UTC())
			if _, err = io.WriteString(w, entry.Log); err != nil {
				log.Fatal(err)
			}
			if written++; written%512 == 0 {
				h.flush(ctx)
			}
		}
		_ = file.Close()
		if err = scanner.Err(); err != nil {
			log.Fatal(err)
		}
	}
	if err := w.Close(); err != nil {
		log.Fatal(err)
	}
	h.flush(ctx)
	return written
}

// flush writes what the writers queued, and seals the heads that filled
func (h *harness) flush(ctx context.Context) {
	if err := h.logs.Flush(ctx); err != nil {
		log.Fatal(err)
	}
	if appended := counted(h.logs.Stats().Appended); appended-h.appended >= 16_384 {
		h.maintain(ctx)
		h.appended = appended
	}
}

// linesCensus follows everything the writers wrote and counts the records of
// several lines and each level found; it returns how many are errors or worse
func linesCensus(ctx context.Context, h *harness) int {
	levels := map[string]int{}
	joined, errors := 0, 0
	cursor := records.Cursor{}
	for {
		batch, err := h.logs.Follow(ctx, cursor, 10_000)
		if err != nil {
			log.Fatal(err)
		}
		if len(batch.Records) == 0 {
			break
		}
		for _, record := range batch.Records {
			if record.Body != nil && strings.Contains(*record.Body, "\n") {
				joined++
			}
			name := "none"
			if record.Level != nil {
				name = record.Level.String()
				if *record.Level >= slog.LevelError {
					errors++
				}
			}
			levels[name]++
		}
		cursor = batch.Next
	}
	fmt.Printf("records_of_several_lines=%d levels=%v\n", joined, levels)
	return errors
}

// askLevel reads every record at error or above, over all time
func askLevel(ctx context.Context, h *harness, want int) {
	ask(ctx, h, "level error or above, all time", records.Query{MinLevel: new(slog.LevelError)}, want)
}
