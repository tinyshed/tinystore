package main

import (
	"bytes"
	"flag"
	"io"
	"strings"
	"testing"
)

// tinystore alone lists its commands by what they are for and is no error;
// help names one command's usage, and refuses a command there is not
func TestHelpListsTheCommandsAndIsNoError(t *testing.T) {
	var out bytes.Buffer
	if err := help(nil, &out); err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"Look", "Run", "Develop", "status", "logs", "serve", "mcp", "migrate", "schema"} {
		if !strings.Contains(out.String(), want) {
			t.Fatalf("the help lacks %q:\n%s", want, out.String())
		}
	}
	if strings.Contains(out.String(), "\x1b[") {
		t.Fatalf("help written to a buffer has colours:\n%q", out.String())
	}

	out.Reset()
	if err := help([]string{"logs"}, &out); err != nil || out.String() != logsUsage+"\n" {
		t.Fatalf("help logs: %q, %v", out.String(), err)
	}
	if err := help([]string{"nothing"}, &out); err == nil {
		t.Fatal("help of a command there is not was no error")
	}
}

// a command's directory comes before its flags, after them or through --dir,
// and is the current directory when none is given
func TestADirectoryComesBeforeOrAfterTheFlags(t *testing.T) {
	for _, c := range []struct {
		args []string
		dir  string
		fail bool
	}{
		{[]string{"./data", "--json"}, "./data", false},
		{[]string{"--json", "./data"}, "./data", false},
		{[]string{"--dir", "./data", "--json"}, "./data", false},
		{[]string{"--json"}, ".", false},
		{[]string{"./a", "./b"}, "", true},
		{[]string{"./a", "--dir", "./b"}, "", true},
	} {
		flags := flag.NewFlagSet("status", flag.ContinueOnError)
		flags.SetOutput(io.Discard)
		given := flags.String("dir", "", "")
		asJSON := flags.Bool("json", false, "")
		dir, err := parseWithDir(flags, c.args, given)
		if c.fail != (err != nil) || !c.fail && (dir != c.dir || !*asJSON) {
			t.Fatalf("%v: %q, json %v, %v", c.args, dir, *asJSON, err)
		}
	}
}
