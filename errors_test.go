package tinystore

import (
	"encoding/json"
	"errors"
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

func TestALimitErrorNamesItsBoundAndIsItsKind(t *testing.T) {
	engines := errors.Join(errors.New("metrics resource limit"), ErrLimit)
	err := error(&LimitError{Name: "decoded samples", Wanted: 120000, Bound: 100000, Kind: engines})
	var reached *LimitError
	if !errors.As(err, &reached) || !errors.Is(err, ErrLimit) || !errors.Is(err, engines) || reached.Wanted <= reached.Bound {
		t.Fatalf("%v: not the limit it reached", err)
	}
	if plain := (&LimitError{Name: "store memory", Wanted: 2, Bound: 1}).Error(); plain != "resource limit: store memory: 2 past 1" {
		t.Fatalf("Error() = %q", plain)
	}
}

// every limit a LimitError names is a constant a package exports, and the
// SDKs' limits are the same names (testdata/limits.json)
func TestEveryLimitNameIsAnExportedConstant(t *testing.T) {
	text, err := os.ReadFile(filepath.Join("testdata", "limits.json"))
	if err != nil {
		t.Fatal(err)
	}
	var file struct {
		Limits []struct{ Go, Name string } `json:"limits"`
	}
	if err = json.Unmarshal(text, &file); err != nil {
		t.Fatal(err)
	}
	listed := map[string]string{}
	for _, limit := range file.Limits {
		listed[limit.Go] = limit.Name
	}

	exported := map[string]string{}
	for _, dir := range []string{".", "metrics", "records", "jobs", "blobs", "kv", "sqldb"} {
		pkg := filepath.Base(dir)
		if dir == "." {
			pkg = "tinystore"
		}
		for _, f := range parsedFiles(t, dir) {
			for name, value := range limitConstants(f) {
				exported[pkg+"."+name] = value
			}
			for _, at := range literalLimitNames(f) {
				t.Errorf("%s: a LimitError named by a literal, not a constant", at)
			}
		}
	}
	for name, value := range exported {
		if listed[name] != value {
			t.Errorf("%s is %q, and testdata/limits.json says %q", name, value, listed[name])
		}
	}
	for name := range listed {
		if _, ok := exported[name]; !ok {
			t.Errorf("testdata/limits.json lists %s, which no package exports", name)
		}
	}
}

func parsedFiles(t *testing.T, dir string) []*ast.File {
	t.Helper()
	names, err := filepath.Glob(filepath.Join(dir, "*.go"))
	if err != nil {
		t.Fatal(err)
	}
	var files []*ast.File
	for _, name := range names {
		if strings.HasSuffix(name, "_test.go") {
			continue
		}
		f, err := parser.ParseFile(token.NewFileSet(), name, nil, 0)
		if err != nil {
			t.Fatal(err)
		}
		files = append(files, f)
	}
	return files
}

// limitConstants is a file's exported string constants named Limit…
func limitConstants(f *ast.File) map[string]string {
	found := map[string]string{}
	for _, decl := range f.Decls {
		gen, ok := decl.(*ast.GenDecl)
		if !ok || gen.Tok != token.CONST {
			continue
		}
		for _, spec := range gen.Specs {
			value := spec.(*ast.ValueSpec)
			for i, name := range value.Names {
				if !strings.HasPrefix(name.Name, "Limit") || i >= len(value.Values) {
					continue
				}
				if lit, ok := value.Values[i].(*ast.BasicLit); ok && lit.Kind == token.STRING {
					found[name.Name], _ = strconv.Unquote(lit.Value)
				}
			}
		}
	}
	return found
}

// literalLimitNames is where a file names a LimitError, or calls metrics'
// limit, with a string literal
func literalLimitNames(f *ast.File) []string {
	var found []string
	ast.Inspect(f, func(n ast.Node) bool {
		switch node := n.(type) {
		case *ast.CompositeLit:
			if !strings.HasSuffix(typeName(node.Type), "LimitError") {
				return true
			}
			for _, element := range node.Elts {
				if kv, ok := element.(*ast.KeyValueExpr); ok && typeName(kv.Key) == "Name" {
					if _, literal := kv.Value.(*ast.BasicLit); literal {
						found = append(found, f.Name.Name)
					}
				}
			}
		case *ast.CallExpr:
			if typeName(node.Fun) == "limit" && len(node.Args) > 0 {
				if _, literal := node.Args[0].(*ast.BasicLit); literal {
					found = append(found, f.Name.Name)
				}
			}
		}
		return true
	})
	return found
}

func typeName(e ast.Expr) string {
	switch x := e.(type) {
	case *ast.Ident:
		return x.Name
	case *ast.SelectorExpr:
		return x.Sel.Name
	}
	return ""
}
