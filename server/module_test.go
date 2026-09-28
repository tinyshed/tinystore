package server

import (
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

// what a program embedding the server links beside the store: nothing
func TestTheServerRequiresOnlyTheRoot(t *testing.T) {
	text, err := os.ReadFile("go.mod")
	if err != nil {
		t.Fatal(err)
	}
	for _, module := range directRequirements(string(text)) {
		if module != "github.com/tinyshed/tinystore" {
			t.Errorf("go.mod requires %s; the server requires the store and nothing else", module)
		}
	}
}

// a client in Go links the bytes without SQLite
func TestWireImportsOnlyTheStandardLibrary(t *testing.T) {
	files, err := filepath.Glob(filepath.Join("wire", "*.go"))
	if err != nil {
		t.Fatal(err)
	}
	for _, file := range files {
		if strings.HasSuffix(file, "_test.go") {
			continue
		}
		parsed, err := parser.ParseFile(token.NewFileSet(), file, nil, parser.ImportsOnly)
		if err != nil {
			t.Fatal(err)
		}
		for _, imported := range parsed.Imports {
			path, err := strconv.Unquote(imported.Path.Value)
			if err != nil {
				t.Fatal(err)
			}
			if first, _, _ := strings.Cut(path, "/"); strings.Contains(first, ".") {
				t.Errorf("%s imports %s; server/wire imports only the standard library", file, path)
			}
			if strings.HasPrefix(path, "github.com/tinyshed/") {
				t.Errorf("%s imports %s, which a client would link", file, path)
			}
		}
	}
}

func directRequirements(goMod string) []string {
	var modules []string
	inBlock := false
	for line := range strings.Lines(goMod) {
		line = strings.TrimSpace(line)
		switch {
		case line == "require (":
			inBlock = true
			continue
		case inBlock && line == ")":
			inBlock = false
			continue
		case strings.Contains(line, "// indirect"), line == "":
			continue
		}
		if after, found := strings.CutPrefix(line, "require "); found {
			line = after
		} else if !inBlock {
			continue
		}
		if path, _, found := strings.Cut(line, " "); found {
			modules = append(modules, path)
		}
	}
	return modules
}
