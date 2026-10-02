package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"iter"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/tinyshed/tinystore/server/reach"
	"github.com/tinyshed/tinystore/server/wire"
)

const statusUsage = `usage:
  tinystore status [dir] [--json]   what a store's directory holds, and the server serving it

It reads the files beside the server and opens no store, so it changes
nothing; --json prints the same for a script.`

// how long status waits for the server SERVE names to prove it answers
const answerWait = time.Second

// directoryStatus is what status prints: each engine's file and the bytes it
// holds with its write-ahead log, the blobs engine's objects, and the server
// SERVE publishes, its secret left out.
type directoryStatus struct {
	Dir    string        `json:"dir"`
	Locked bool          `json:"locked"` // a LOCK file is there; only the store holding it knows it is held
	Files  []fileStatus  `json:"files"`
	Blobs  *blobsStatus  `json:"blobs,omitempty"`
	Server *serverStatus `json:"server,omitempty"`
}

type fileStatus struct {
	Name  string `json:"name"`
	Bytes int64  `json:"bytes"`
}

type blobsStatus struct {
	Files int   `json:"files"`
	Bytes int64 `json:"bytes"`
}

type serverStatus struct {
	Protocol  int       `json:"protocol"`
	Version   string    `json:"version"`
	PID       int       `json:"pid"`
	Endpoints []string  `json:"endpoints"`
	Since     time.Time `json:"since"`   // when SERVE was written, as its server began
	Answers   bool      `json:"answers"` // it proved it read SERVE; a crash leaves one that does not
}

// status prints what a store's directory holds without opening the store, so
// that it runs beside the sidecar serving it and changes nothing.
func status(ctx context.Context, args []string, out io.Writer, stderr io.Writer) error {
	flags := flag.NewFlagSet("status", flag.ContinueOnError)
	flags.SetOutput(stderr)
	flags.Usage = func() { fmt.Fprintln(stderr, statusUsage) }
	given := flags.String("dir", "", "")
	asJSON := flags.Bool("json", false, "")
	dir, err := parseWithDir(flags, args, given)
	if err != nil {
		return err
	}

	found, err := readStatus(dir)
	if err != nil {
		return err
	}
	if found.Server != nil {
		found.Server.Answers = answers(ctx, dir)
	}
	if *asJSON {
		encoder := json.NewEncoder(out)
		encoder.SetIndent("", "  ")
		return encoder.Encode(found)
	}
	p := paint(false)
	if file, ok := out.(*os.File); ok {
		p = paint(colors(file))
	}
	_, err = io.WriteString(out, showStatus(found, p, time.Now()))
	return err
}

// answers says that the server SERVE names proves it read it
func answers(ctx context.Context, dir string) bool {
	ctx, cancel := context.WithTimeout(ctx, answerWait)
	defer cancel()
	conn, err := reach.Found(ctx, dir, clientName())
	if err != nil {
		return false
	}
	_ = conn.Close() // it answered, which is all status asked
	return true
}

// showStatus is what a person reads of a directory:
//
//	● ./data  served by v0.1.0 · pid 28308 · up 2h 14m
//
//	  kv          4.0 MiB
//	  jobs      877.0 KiB
//	  ─────────────────
//	  total       4.9 MiB
func showStatus(found directoryStatus, p paint, now time.Time) string {
	var text strings.Builder
	switch server := found.Server; {
	case server == nil:
		fmt.Fprintf(&text, "%s %s  %s\n", p.in(dim, "○"), p.in(bold, found.Dir), p.in(dim, "served by none"))
	case !server.Answers:
		fmt.Fprintf(&text, "%s %s  %s\n", p.in(yellow, "●"), p.in(bold, found.Dir),
			p.in(yellow, fmt.Sprintf("SERVE names pid %d, which does not answer", server.PID)))
	default:
		fmt.Fprintf(&text, "%s %s  served by %s %s\n", p.in(green, "●"), p.in(bold, found.Dir), server.Version,
			p.in(dim, fmt.Sprintf("· pid %d · up %s", server.PID, uptime(now.Sub(server.Since)))))
	}
	text.WriteString("\n")
	var total int64
	line := func(name string, bytes int64, more string) {
		total += bytes
		fmt.Fprintf(&text, "  %s %10s", p.in(cyan, fmt.Sprintf("%-10s", name)), size(bytes))
		if more != "" {
			fmt.Fprintf(&text, "  %s", p.in(dim, more))
		}
		text.WriteString("\n")
	}
	for _, file := range found.Files {
		line(strings.TrimSuffix(file.Name, ".db"), file.Bytes, "")
	}
	if found.Blobs != nil {
		line("blobs", found.Blobs.Bytes, fmt.Sprintf("%d files", found.Blobs.Files))
	}
	fmt.Fprintf(&text, "  %s\n  %-10s %10s\n", p.in(dim, strings.Repeat("─", 22)), "total", size(total))
	return text.String()
}

// size is bytes as a person reads them: 877.0 KiB, 4.0 MiB
func size(bytes int64) string {
	value, unit := float64(bytes), 0
	for value >= 1024 && unit < 4 {
		value, unit = value/1024, unit+1
	}
	if unit == 0 {
		return fmt.Sprintf("%d B", bytes)
	}
	return fmt.Sprintf("%.1f %s", value, []string{"", "KiB", "MiB", "GiB", "TiB"}[unit])
}

// uptime is how long a server has run, to the minute: 2h 14m, 3d 4h
func uptime(d time.Duration) string {
	d = d.Round(time.Minute)
	days, hours, minutes := int(d/(24*time.Hour)), int(d/time.Hour)%24, int(d/time.Minute)%60
	switch {
	case days > 0:
		return fmt.Sprintf("%dd %dh", days, hours)
	case hours > 0:
		return fmt.Sprintf("%dh %dm", hours, minutes)
	}
	return fmt.Sprintf("%dm", minutes)
}

func readStatus(dir string) (directoryStatus, error) {
	found := directoryStatus{Dir: dir, Files: []fileStatus{}}
	entries, err := os.ReadDir(dir)
	if err != nil {
		return found, err
	}
	if !slices.ContainsFunc(entries, func(entry os.DirEntry) bool { return entry.Name() == "LOCK" }) {
		return found, fmt.Errorf("%s holds no LOCK, which every store's directory does", dir)
	}
	for _, entry := range entries {
		switch name := entry.Name(); {
		case name == "LOCK":
			found.Locked = true
		case strings.HasSuffix(name, ".db"):
			found.Files = append(found.Files, fileStatus{Name: name, Bytes: withLog(filepath.Join(dir, name))})
		case name == "sql" && entry.IsDir():
			found.Files = append(found.Files, databases(dir)...)
		case name == "blobs" && entry.IsDir():
			found.Blobs = objects(filepath.Join(dir, name))
		}
	}
	slices.SortFunc(found.Files, func(a, b fileStatus) int { return strings.Compare(a.Name, b.Name) })
	found.Server = published(dir)
	return found, nil
}

// stat reads a file of the directory a person named
func stat(path string) (os.FileInfo, error) {
	return os.Stat(path) //nolint:gosec // reading the directory it is given is what status is for
}

// withLog is a database's bytes with its write-ahead log and shared memory
func withLog(path string) int64 {
	var total int64
	for _, suffix := range []string{"", "-wal", "-shm"} {
		if info, err := stat(path + suffix); err == nil {
			total += info.Size()
		}
	}
	return total
}

func databases(dir string) []fileStatus {
	names, err := filepath.Glob(filepath.Join(dir, "sql", "*.db"))
	if err != nil {
		return nil
	}
	files := make([]fileStatus, 0, len(names))
	for _, name := range names {
		files = append(files, fileStatus{Name: "sql/" + filepath.Base(name), Bytes: withLog(name)})
	}
	return files
}

// objects counts the blobs engine's files, its database among them; one gone
// while it walks, as the store beside it removes them, is not counted
func objects(dir string) *blobsStatus {
	total := &blobsStatus{}
	for path := range walked(dir) {
		if info, err := stat(path); err == nil {
			total.Files++
			total.Bytes += info.Size()
		}
	}
	return total
}

// walked is every file under dir that could be listed
func walked(dir string) iter.Seq[string] {
	return func(yield func(string) bool) {
		entries, err := os.ReadDir(dir)
		if err != nil {
			return
		}
		for _, entry := range entries {
			path := filepath.Join(dir, entry.Name())
			if entry.IsDir() {
				for inner := range walked(path) {
					if !yield(inner) {
						return
					}
				}
			} else if !yield(path) {
				return
			}
		}
	}
}

// published is the server SERVE names, nil when none does; its secret never
// leaves the file
func published(dir string) *serverStatus {
	path := filepath.Join(dir, "server", "SERVE")
	text, err := os.ReadFile(path) //nolint:gosec // the directory asked about
	if err != nil {
		return nil
	}
	var serve wire.Published
	if json.Unmarshal(text, &serve) != nil {
		return nil
	}
	found := &serverStatus{Protocol: serve.Protocol, Version: serve.Server, PID: serve.PID, Endpoints: serve.Endpoints}
	if info, err := stat(path); err == nil {
		found.Since = info.ModTime()
	}
	return found
}
