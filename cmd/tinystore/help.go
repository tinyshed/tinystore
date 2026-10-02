package main

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"strings"
)

// a command the help lists: its name, what follows it, and what it does
type helpLine struct {
	name, args, does string
}

var helpGroups = []struct {
	title string
	lines []helpLine
}{
	{"Look", []helpLine{
		{"status", "[dir]", "what the store holds and who serves it"},
		{"logs", "[dir] -f", "the application's logs, as they arrive"},
	}},
	{"Run", []helpLine{
		{"serve", "<dir>", "share the store with other processes, until Ctrl+C"},
		{"stop", "[dir]", "stop the server of a directory, its work finished"},
		{"mcp", "[dir]", "let an AI agent read the store"},
	}},
	{"Develop", []helpLine{
		{"migrate", "[name]", "compare a schema with its migrations"},
		{"schema", "[name]", "print a schema's SQL"},
	}},
}

// commandUsages is what tinystore help <command> prints
var commandUsages = map[string]string{
	"status":  statusUsage,
	"logs":    logsUsage,
	"serve":   serveUsage,
	"stop":    stopUsage,
	"mcp":     mcpUsage,
	"migrate": usage,
	"schema":  usage,
	"version": "usage:\n  tinystore version   print the release, the Go it was built with and the platform",
}

// printHelp prints the commands in their groups, in colour on a terminal
func printHelp(out io.Writer) {
	p := painterFor(out)
	var text strings.Builder
	fmt.Fprintf(&text, "%s %s · kv, jobs, blobs, SQL, logs and metrics in one directory\n",
		p.in(bold, "tinystore"), p.in(dim, version()))
	for _, group := range helpGroups {
		fmt.Fprintf(&text, "\n%s\n", p.in(bold, group.title))
		for _, line := range group.lines {
			fmt.Fprintf(&text, "  %s %s %s\n", p.in(cyan, fmt.Sprintf("%-7s", line.name)),
				p.in(dim, fmt.Sprintf("%-9s", line.args)), line.does)
		}
	}
	fmt.Fprintf(&text, "\n%s\n", p.in(dim, "tinystore help <command> · --json for scripts"))
	_, _ = io.WriteString(out, text.String()) //nolint:errcheck // help a terminal refuses has nobody to tell
}

// help prints the commands, or one command's usage
func help(args []string, out io.Writer) error {
	if len(args) == 0 {
		printHelp(out)
		return nil
	}
	text, known := commandUsages[args[0]]
	if !known || len(args) > 1 {
		return fmt.Errorf("no command %q: tinystore help lists them", strings.Join(args, " "))
	}
	_, err := fmt.Fprintln(out, text)
	return err
}

// parseWithDir parses a command's flags around one directory, before them or
// after, which --dir may name instead; none is the current directory
func parseWithDir(flags *flag.FlagSet, args []string, dirFlag *string) (string, error) {
	var positional []string
	for {
		if err := flags.Parse(args); err != nil {
			return "", err
		}
		args = flags.Args()
		if len(args) == 0 {
			break
		}
		positional, args = append(positional, args[0]), args[1:]
	}
	switch {
	case len(positional) > 1:
		return "", fmt.Errorf("%s takes one directory, not %s", flags.Name(), strings.Join(positional, " "))
	case len(positional) == 1 && *dirFlag != "":
		return "", errors.New(flags.Name() + " takes a directory or --dir, not both")
	case len(positional) == 1:
		return positional[0], nil
	case *dirFlag != "":
		return *dirFlag, nil
	}
	return ".", nil
}
