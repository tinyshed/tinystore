package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"log"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/tinyshed/tinystore/records"
)

// a docker json-file corpus as bench/fetch-docker-logs.sh leaves it:
//
//	<corpus>/<host>/containers.txt    /name|image|log path
//	<corpus>/<host>/raw/<id>/<id>-json.log, .1, .2 …
//
// every entry becomes one record, as the research round read them: docker's
// receive time, the container as its stream, docker's stream as its name, and
// the line as attributes when it is a JSON object that rebuilds byte for
// byte, or else as its body
type container struct {
	name    string
	records []records.Record
	skipped int
}

// readCorpus reads every container of every host, sorted by name; the names
// are production data and are never printed
func readCorpus(root string) []container {
	if root == "" {
		log.Fatal("-corpus is required")
	}
	hosts, err := os.ReadDir(root)
	if err != nil {
		log.Fatal(err)
	}
	var containers []container
	for _, host := range hosts {
		for id, name := range containerNames(filepath.Join(root, host.Name())) {
			found, err := readContainer(filepath.Join(root, host.Name(), "raw", id), host.Name()+"/"+name)
			if err != nil {
				log.Fatal(err)
			}
			if len(found.records) > 0 {
				containers = append(containers, found)
			}
		}
	}
	slices.SortFunc(containers, func(a, b container) int { return strings.Compare(a.name, b.name) })
	return containers
}

// containerNames maps a container's id to its name, from containers.txt
func containerNames(host string) map[string]string {
	names := map[string]string{}
	data, err := os.ReadFile(filepath.Join(host, "containers.txt")) //nolint:gosec // the corpus the command was given
	if err != nil {
		return names
	}
	for _, line := range strings.Split(strings.TrimSpace(string(data)), "\n") {
		parts := strings.Split(strings.TrimSpace(line), "|")
		if len(parts) == 3 {
			names[filepath.Base(filepath.Dir(parts[2]))] = strings.TrimPrefix(parts[0], "/")
		}
	}
	return names
}

// logFiles lists a container's log files, oldest rotation first
func logFiles(dir string) []string {
	files, err := filepath.Glob(filepath.Join(dir, "*-json.log*"))
	if err != nil {
		log.Fatal(err)
	}
	rotation := func(path string) int {
		if at := strings.LastIndex(path, ".log."); at >= 0 {
			if value, err := strconv.Atoi(path[at+5:]); err == nil {
				return value
			}
		}
		return 0
	}
	slices.SortFunc(files, func(a, b string) int { return rotation(b) - rotation(a) })
	return files
}

// readContainer joins the pieces docker splits lines longer than 16 KiB into;
// an entry padded with NUL by an unclean shutdown does not parse and is skipped
func readContainer(dir, name string) (container, error) {
	found := container{name: name}
	var pending strings.Builder
	for _, path := range logFiles(dir) {
		file, err := os.Open(path) //nolint:gosec // a log file of the corpus the command was given
		if err != nil {
			return found, err
		}
		scanner := bufio.NewScanner(file)
		scanner.Buffer(make([]byte, 64<<10), 16<<20)
		for scanner.Scan() {
			var entry struct{ Log, Stream, Time string }
			if json.Unmarshal(scanner.Bytes(), &entry) != nil {
				found.skipped++
				continue
			}
			pending.WriteString(entry.Log)
			if !strings.HasSuffix(entry.Log, "\n") {
				continue
			}
			found.add(entry.Time, entry.Stream, strings.TrimSuffix(pending.String(), "\n"))
			pending.Reset()
		}
		_ = file.Close()
		if err = scanner.Err(); err != nil {
			return found, err
		}
	}
	return found, nil
}

func (c *container) add(received, stream, text string) {
	at, err := time.Parse(time.RFC3339Nano, received)
	if err != nil {
		c.skipped++
		return
	}
	record := records.Record{At: at.UTC(), Stream: c.name, Name: stream}
	if fields, ok := exactJSON(text); ok {
		record.Attrs = fields
	} else {
		record.Body = &text
	}
	if len(record.Attrs) > 128 || inputSize(&record) > 256<<10 {
		c.skipped++
		return
	}
	c.records = append(c.records, record)
}

// inputSize is what the engine weighs a record at against its bounds
func inputSize(r *records.Record) int {
	size := 32 + len(r.Stream) + len(r.Name)
	if r.Body != nil {
		size += len(*r.Body)
	}
	for _, field := range r.Attrs {
		size += 4 + len(field.Key) + len(field.Value)
	}
	return size
}

// exactJSON is a line's fields when the line is a JSON object they rebuild
// byte for byte, so that storing the fields loses nothing of the line
func exactJSON(text string) ([]records.Field, bool) {
	if len(text) < 2 || text[0] != '{' {
		return nil, false
	}
	fields, err := objectFields([]byte(text))
	if err != nil || len(fields) == 0 {
		return nil, false
	}
	var rebuilt strings.Builder
	rebuilt.WriteByte('{')
	for i, field := range fields {
		if i > 0 {
			rebuilt.WriteByte(',')
		}
		key, err := json.Marshal(field.Key)
		if err != nil {
			return nil, false
		}
		rebuilt.Write(key)
		rebuilt.WriteByte(':')
		rebuilt.WriteString(field.Value)
	}
	rebuilt.WriteByte('}')
	return fields, rebuilt.String() == text
}

// objectFields reads a JSON object's fields in order, each value as it is spelled
func objectFields(data []byte) ([]records.Field, error) {
	decoder := json.NewDecoder(bytes.NewReader(data))
	opening, err := decoder.Token()
	if err != nil || opening != json.Delim('{') {
		return nil, errors.New("not an object")
	}
	var fields []records.Field
	for decoder.More() {
		key, keyErr := decoder.Token()
		if keyErr != nil {
			return nil, keyErr
		}
		name, ok := key.(string)
		if !ok {
			return nil, errors.New("a key that is not a string")
		}
		var value json.RawMessage
		if err = decoder.Decode(&value); err != nil {
			return nil, err
		}
		fields = append(fields, records.Field{Key: name, Value: string(value)})
	}
	if _, err = decoder.Token(); err != nil {
		return nil, err
	}
	if decoder.Decode(new(any)) != io.EOF {
		return nil, errors.New("trailing data")
	}
	return fields, nil
}
