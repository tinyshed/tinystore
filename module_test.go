package tinystore

import (
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

// what this module requires, every program importing it links, see AGENTS.md
var theEngineMayRequire = map[string]bool{
	"github.com/klauspost/compress": true,
	"modernc.org/sqlite":            true,
}

func TestTheModuleCarriesOnlyTheEngine(t *testing.T) {
	text, err := os.ReadFile("go.mod")
	if err != nil {
		t.Fatal(err)
	}

	for _, module := range directRequirements(string(text)) {
		if !theEngineMayRequire[module] {
			t.Errorf("go.mod requires %s directly; a measurement's dependency belongs in bench/", module)
		}
	}
}

func TestNothingHereImportsItsCaller(t *testing.T) {
	for _, file := range goFiles(t) {
		parsed, err := parser.ParseFile(token.NewFileSet(), file, nil, parser.ImportsOnly)
		if err != nil {
			t.Fatal(err)
		}
		for _, imported := range parsed.Imports {
			path, err := strconv.Unquote(imported.Path.Value)
			if err != nil {
				t.Fatal(err)
			}
			ours := path == "github.com/tinyshed/tinystore" || strings.HasPrefix(path, "github.com/tinyshed/tinystore/")
			if strings.HasPrefix(path, "github.com/tinyshed/") && !ours {
				t.Errorf("%s imports %s; the store knows samples, series, labels and time", file, path)
			}
		}
	}
}

func goFiles(t *testing.T) []string {
	t.Helper()

	var files []string
	err := filepath.WalkDir(".", func(path string, entry os.DirEntry, err error) error {
		if err == nil && !entry.IsDir() && strings.HasSuffix(path, ".go") {
			files = append(files, path)
		}
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return files
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

// the root imports no engine, so a program links only the engines it opens
func TestTheRootImportsNoEngine(t *testing.T) {
	files, err := filepath.Glob("*.go")
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
		for _, entry := range parsed.Imports {
			path, err := strconv.Unquote(entry.Path.Value)
			if err != nil {
				t.Fatal(err)
			}
			if strings.HasPrefix(path, "github.com/tinyshed/tinystore/") &&
				!strings.HasPrefix(path, "github.com/tinyshed/tinystore/internal/") {
				t.Errorf("%s imports %s; the root may import only internal/", file, path)
			}
		}
	}
}

// engines never import each other, so a program links only the engines it opens
func TestEnginesDoNotImportEachOther(t *testing.T) {
	engines := []string{"metrics", "records", "sqldb"}
	for _, engine := range engines {
		files, err := filepath.Glob(filepath.Join(engine, "*.go"))
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
			for _, entry := range parsed.Imports {
				path, err := strconv.Unquote(entry.Path.Value)
				if err != nil {
					t.Fatal(err)
				}
				for _, other := range engines {
					if other != engine && path == "github.com/tinyshed/tinystore/"+other {
						t.Errorf("%s imports the %s engine", file, other)
					}
				}
			}
		}
	}
}
