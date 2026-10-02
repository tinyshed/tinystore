package main

import (
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"iter"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"github.com/tinyshed/tinystore/server/wire"
)

const statusUsage = `usage:
  tinystore status --dir <dir>   what a store's directory holds, as JSON, read beside the server serving it`

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
	Protocol  int      `json:"protocol"`
	Version   string   `json:"version"`
	PID       int      `json:"pid"`
	Endpoints []string `json:"endpoints"`
}

// status prints what a store's directory holds without opening the store, so
// that it runs beside the sidecar serving it and changes nothing.
func status(args []string, out io.Writer, stderr io.Writer) error {
	flags := flag.NewFlagSet("status", flag.ContinueOnError)
	flags.SetOutput(stderr)
	dir := flags.String("dir", "", "")
	if err := flags.Parse(args); err != nil {
		return err
	}
	if *dir == "" || flags.NArg() > 0 {
		return errors.New(statusUsage)
	}

	found, err := readStatus(*dir)
	if err != nil {
		return err
	}
	encoder := json.NewEncoder(out)
	encoder.SetIndent("", "  ")
	return encoder.Encode(found)
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

// withLog is a database's bytes with its write-ahead log and shared memory
func withLog(path string) int64 {
	var total int64
	for _, suffix := range []string{"", "-wal", "-shm"} {
		if info, err := os.Stat(path + suffix); err == nil {
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
		if info, err := os.Stat(path); err == nil {
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
	text, err := os.ReadFile(filepath.Join(dir, "server", "SERVE")) //nolint:gosec // the directory asked about
	if err != nil {
		return nil
	}
	var serve wire.Published
	if json.Unmarshal(text, &serve) != nil {
		return nil
	}
	return &serverStatus{Protocol: serve.Protocol, Version: serve.Server, PID: serve.PID, Endpoints: serve.Endpoints}
}
