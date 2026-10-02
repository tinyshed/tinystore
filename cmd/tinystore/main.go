// Command tinystore is TinyStore's one executable. It serves a store to other
// processes, and its sqldb commands each find the test that checks a
// database, run it, and print what it answered, so that the comparison and the
// migration it writes come from the sqldb the application pinned, never from
// this tool's own.
//
//	tinystore serve --dir <dir> --stdio | --local | --listen <endpoint>
//	tinystore status --dir <dir>                   what a store's directory holds, as JSON
//	tinystore version                              the release, the Go it was built with, the platform
//	go tool tinystore migrate [name]               what differs; writes nothing
//	go tool tinystore migrate [name] new <what>    write the next migration from the difference
//	go tool tinystore schema [name]                the schema's SQL
//
// A database's name is the one sqldb.Open takes and sqldbtest.CheckSchema is
// given; it is needed only when the module checks more than one.
package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"
)

// the environment variable sqldbtest.CheckSchema reads a request from
const requestVariable = "TINYSTORE_SQLDB"

const usage = `usage:
  tinystore serve --dir <dir> ...        serve the store in dir to other processes; tinystore serve -h says how
  tinystore migrate [name]               what differs between a schema and its migrations; writes nothing
  tinystore migrate [name] new <what>    write the next migration from the difference
  tinystore schema [name]                print the schema's SQL
  tinystore status --dir <dir>           what a store's directory holds, as JSON, beside its server
  tinystore version                      print the release, the Go it was built with and the platform`

func main() {
	if len(os.Args) > 1 && os.Args[1] == "serve" {
		ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
		err := serve(ctx, os.Args[2:], console{stdin: os.Stdin, stdout: os.Stdout, stderr: os.Stderr})
		stop()
		exit(err)
	}
	if len(os.Args) > 1 && os.Args[1] == "status" {
		exit(status(os.Args[2:], os.Stdout, os.Stderr))
	}
	if len(os.Args) == 2 && os.Args[1] == "version" {
		fmt.Printf("tinystore %s (%s %s/%s)\n", version(), runtime.Version(), runtime.GOOS, runtime.GOARCH)
		os.Exit(0)
	}
	exit(run(context.Background(), os.Args[1:], os.Stdout))
}

// exit ends the process as err says.
//
// A directory held by another store has a code of its own, which a client
// starting a sidecar reads as another having won.
func exit(err error) {
	switch {
	case err == nil, errors.Is(err, flag.ErrHelp):
		os.Exit(0)
	case errors.Is(err, errHeld):
		fmt.Fprintln(os.Stderr, "tinystore:", err)
		os.Exit(exitHeld)
	}
	fmt.Fprintln(os.Stderr, "tinystore:", err)
	os.Exit(1)
}

// command is what the arguments ask of sqldbtest: migrate, new or schema
type command struct {
	verb     string
	database string
	what     string
}

func parse(args []string) (command, error) {
	if len(args) == 0 {
		return command{}, errors.New(usage)
	}
	verb, rest := args[0], args[1:]
	parsed := command{verb: verb}
	if len(rest) > 0 && rest[0] != "new" {
		parsed.database, rest = rest[0], rest[1:]
	}
	switch {
	case verb == "schema" && len(rest) == 0:
	case verb == "migrate" && len(rest) == 0:
	case verb == "migrate" && len(rest) == 2 && rest[0] == "new":
		parsed.verb, parsed.what = "new", rest[1]
	default:
		return command{}, errors.New(usage)
	}
	return parsed, nil
}

func run(ctx context.Context, args []string, out io.Writer) error {
	asked, err := parse(args)
	if err != nil {
		return err
	}
	here, err := os.Getwd()
	if err != nil {
		return err
	}
	root, err := moduleRoot(here)
	if err != nil {
		return err
	}
	checks, unnamed, err := findChecks(root)
	if err != nil {
		return err
	}
	chosen, err := pick(checks, unnamed, asked.database, asked.verb == "migrate")
	if err != nil {
		return err
	}
	for _, c := range chosen {
		given, err := ask(ctx, root, c, asked)
		if err != nil {
			return err
		}
		if err = show(out, here, asked, given); err != nil {
			return err
		}
	}
	return nil
}

// answer is what a check wrote for the tool, as sqldbtest spells it
type answer struct {
	Database    string   `json:"database"`
	Dir         string   `json:"dir"`
	Migrations  int      `json:"migrations"`
	Differences []string `json:"differences"`
	Notes       []string `json:"notes"`
	Wrote       string   `json:"wrote"`
	SQL         string   `json:"sql"`
	Unfinished  bool     `json:"unfinished"`
	Schema      string   `json:"schema"`
	Error       string   `json:"error"`
}

// ask runs the test that calls a check, with the request in its environment,
// and reads what the check answered
func ask(ctx context.Context, root string, c check, asked command) (answer, error) {
	if c.test == "" {
		return answer{}, fmt.Errorf("the check of %q at %s is in a helper; "+
			"call it from a Test function, which go test -run can name", c.database, c.at)
	}
	scratch, err := os.MkdirTemp("", "tinystore-")
	if err != nil {
		return answer{}, err
	}
	defer func() { _ = os.RemoveAll(scratch) }()
	answers := filepath.Join(scratch, "answer.jsonl")
	request, err := json.Marshal(map[string]string{
		"database": c.database, "command": asked.verb, "what": asked.what, "answer": answers,
	})
	if err != nil {
		return answer{}, err
	}
	pkg, err := filepath.Rel(root, c.dir)
	if err != nil {
		return answer{}, err
	}

	args := []string{"test", "-count=1", "-run", "^" + c.test + "$", "./" + filepath.ToSlash(pkg)}
	test := exec.CommandContext(ctx, "go", args...) //nolint:gosec // running the module's own test is the tool's job
	test.Dir, test.Env = root, append(os.Environ(), requestVariable+"="+string(request))
	output, testErr := test.CombinedOutput()
	if testErr != nil {
		return answer{}, fmt.Errorf("go test ./%s for %q failed: %w\n%s", filepath.ToSlash(pkg), c.database,
			testErr, output)
	}

	given, err := readAnswer(answers, c.database)
	if err != nil {
		return answer{}, fmt.Errorf("go test ./%s answered nothing for %q: %w\n%s", filepath.ToSlash(pkg), c.database,
			err, output)
	}
	return given, nil
}

func readAnswer(path, database string) (answer, error) {
	text, err := os.ReadFile(path) //nolint:gosec // the file ask made for the answer
	if err != nil {
		return answer{}, err
	}
	for line := range strings.Lines(string(text)) {
		var given answer
		if err = json.Unmarshal([]byte(line), &given); err == nil && given.Database == database {
			return given, nil
		}
	}
	return answer{}, errors.New("no answer of that name")
}

// show prints what a check answered, as the command asked
func show(out io.Writer, here string, asked command, given answer) error {
	if given.Error != "" {
		return fmt.Errorf("%s: %s", given.Database, given.Error)
	}
	var err error
	switch asked.verb {
	case "schema":
		_, err = fmt.Fprint(out, given.Schema)
	case "new":
		err = showWritten(out, here, given)
	default:
		err = showDifferences(out, given)
	}
	return err
}

func showWritten(out io.Writer, here string, given answer) error {
	if given.Wrote == "" {
		_, err := fmt.Fprintf(out, "%s: its migrations make what the schema declares; nothing to write\n",
			given.Database)
		return err
	}
	path := given.Wrote
	if relative, err := filepath.Rel(here, path); err == nil {
		path = filepath.ToSlash(relative)
	}
	_, err := fmt.Fprintf(out, "wrote %s:\n\n%s\n", path, indent(given.SQL))
	if err == nil && given.Unfinished {
		_, err = fmt.Fprintln(out, "it is a draft: write what each TODO asks, then run go test")
	}
	return err
}

// showDifferences prints a database's migrations against its schema:
//
//	app: 2 migrations, and the schema matches them
func showDifferences(out io.Writer, given answer) error {
	var text strings.Builder
	migrations := fmt.Sprintf("%d migrations", given.Migrations)
	if given.Migrations == 1 {
		migrations = "1 migration"
	}
	if len(given.Differences) == 0 {
		fmt.Fprintf(&text, "%s: %s, and the schema matches them\n", given.Database, migrations)
	} else {
		fmt.Fprintf(&text, "%s: %s, and the schema differs:\n", given.Database, migrations)
		for _, line := range given.Differences {
			fmt.Fprintf(&text, "  %s\n", line)
		}
		fmt.Fprintf(&text, "write it with: go tool tinystore migrate %s new <what>\n", given.Database)
	}
	for _, note := range given.Notes {
		fmt.Fprintf(&text, "  note: %s\n", note)
	}
	_, err := io.WriteString(out, text.String())
	return err
}

func indent(sql string) string {
	var out strings.Builder
	for line := range strings.Lines(sql) {
		if strings.TrimSpace(line) == "" {
			out.WriteString(line)
			continue
		}
		out.WriteString("    " + line)
	}
	return out.String()
}
