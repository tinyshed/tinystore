package metrics

import (
	"go/parser"
	"go/token"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

func TestEngineDoesNotImportExperimentsOrFutureEngines(t *testing.T) {
	files, err := filepath.Glob("*.go")
	if err != nil {
		t.Fatal(err)
	}
	for _, file := range files {
		if strings.HasSuffix(file, "_test.go") {
			continue
		}
		parsed, parseErr := parser.ParseFile(token.NewFileSet(), file, nil, parser.ImportsOnly)
		if parseErr != nil {
			t.Fatal(parseErr)
		}
		for _, entry := range parsed.Imports {
			path, unquoteErr := strconv.Unquote(entry.Path.Value)
			if unquoteErr != nil {
				t.Fatal(unquoteErr)
			}
			for _, forbidden := range []string{"/spike", "/bench", "/records", "/kv", "/query"} {
				if strings.HasPrefix(path, "github.com/tinyshed/tinystore"+forbidden) {
					t.Errorf("%s crosses the engine boundary through %s", file, path)
				}
			}
		}
	}
}
