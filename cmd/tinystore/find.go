package main

import (
	"errors"
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"io/fs"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
)

// the package whose CheckSchema a program's test calls, a check a database
const sqldbtestPath = "github.com/tinyshed/tinystore/sqldb/sqldbtest"

// check is one call of sqldbtest.CheckSchema: the database it checks, the test
// function that calls it and the package that test is in
type check struct {
	database string
	test     string // "" when a helper calls it rather than a test
	dir      string // the package's directory
	at       string // file:line, as errors name it
}

// findChecks reads every test file of the module at root, its nested modules
// left out, for the checks it calls; a check whose name is not a literal is
// one no command can find by name
func findChecks(root string) (checks []check, unnamed []string, err error) {
	err = filepath.WalkDir(root, func(path string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if entry.IsDir() {
			return skipDirectory(root, path, entry.Name())
		}
		if !strings.HasSuffix(path, "_test.go") {
			return nil
		}
		found, anonymous, parseErr := checksIn(path)
		checks, unnamed = append(checks, found...), append(unnamed, anonymous...)
		return parseErr
	})
	sort.Slice(checks, func(i, j int) bool { return checks[i].at < checks[j].at })
	return checks, unnamed, err
}

// skipDirectory leaves out what no package of the module lies in: another
// module, testdata, vendor, and hidden directories
func skipDirectory(root, path, name string) error {
	if path == root {
		return nil
	}
	if name == "testdata" || name == "vendor" || strings.HasPrefix(name, ".") || strings.HasPrefix(name, "_") {
		return filepath.SkipDir
	}
	if _, err := os.Stat(filepath.Join(path, "go.mod")); err == nil {
		return filepath.SkipDir
	}
	return nil
}

// checksIn is the checks one test file calls
func checksIn(path string) (checks []check, unnamed []string, err error) {
	files := token.NewFileSet()
	file, err := parser.ParseFile(files, path, nil, parser.SkipObjectResolution)
	if err != nil {
		return nil, nil, err
	}
	local := importName(file)
	if local == "" {
		return nil, nil, nil
	}
	for _, declared := range file.Decls {
		function, ok := declared.(*ast.FuncDecl)
		if !ok || function.Body == nil {
			continue
		}
		test := ""
		if function.Recv == nil && strings.HasPrefix(function.Name.Name, "Test") {
			test = function.Name.Name
		}
		ast.Inspect(function.Body, func(node ast.Node) bool {
			call, isCall := node.(*ast.CallExpr)
			if !isCall || !callsCheck(call, local) {
				return true
			}
			at := files.Position(call.Pos())
			place := fmt.Sprintf("%s:%d", at.Filename, at.Line)
			database, named := literalName(call)
			if !named {
				unnamed = append(unnamed, place)
				return true
			}
			checks = append(checks, check{database: database, test: test, dir: filepath.Dir(path), at: place})
			return true
		})
	}
	return checks, unnamed, nil
}

// importName is what a file calls sqldbtest by, "." for a dot import, or ""
// when it does not import it
func importName(file *ast.File) string {
	for _, imported := range file.Imports {
		if path, err := strconv.Unquote(imported.Path.Value); err != nil || path != sqldbtestPath {
			continue
		}
		if imported.Name != nil {
			return imported.Name.Name
		}
		return "sqldbtest"
	}
	return ""
}

func callsCheck(call *ast.CallExpr, local string) bool {
	switch fun := call.Fun.(type) {
	case *ast.SelectorExpr:
		owner, ok := fun.X.(*ast.Ident)
		return ok && owner.Name == local && fun.Sel.Name == "CheckSchema"
	case *ast.Ident:
		return local == "." && fun.Name == "CheckSchema"
	}
	return false
}

func literalName(call *ast.CallExpr) (string, bool) {
	if len(call.Args) < 2 {
		return "", false
	}
	text, ok := call.Args[1].(*ast.BasicLit)
	if !ok || text.Kind != token.STRING {
		return "", false
	}
	name, err := strconv.Unquote(text.Value)
	return name, err == nil
}

// moduleRoot is the directory of the go.mod the working directory lies in
func moduleRoot(from string) (string, error) {
	for dir := from; ; dir = filepath.Dir(dir) {
		if _, err := os.Stat(filepath.Join(dir, "go.mod")); err == nil {
			return dir, nil
		}
		if filepath.Dir(dir) == dir {
			return "", errors.New("no go.mod here or above: run it inside the module whose tests check its databases")
		}
	}
}

// pick is the checks a command acts on: the named database's, or every one
// when none is named and the command allows it; one name checked twice in the
// module is an error, as sqldbtest refuses it in one test binary
func pick(checks []check, unnamed []string, database string, many bool) ([]check, error) {
	byName := map[string]check{}
	var names []string
	for _, c := range checks {
		if first, twice := byName[c.database]; twice {
			return nil, fmt.Errorf("%q is checked twice, at %s and at %s; one check a database",
				c.database, first.at, c.at)
		}
		byName[c.database] = c
		names = append(names, c.database)
	}
	switch {
	case database != "":
		c, found := byName[database]
		if !found {
			return nil, notFound(database, names, unnamed)
		}
		return []check{c}, nil
	case len(checks) == 0:
		return nil, notFound("", nil, unnamed)
	case len(checks) > 1 && !many:
		return nil, fmt.Errorf("the module checks %s; name one", strings.Join(names, ", "))
	}
	return checks, nil
}

func notFound(database string, names, unnamed []string) error {
	what := "no test calls sqldbtest.CheckSchema"
	if database != "" {
		what = fmt.Sprintf("no test checks %q", database)
		if len(names) > 0 {
			what += "; the module checks " + strings.Join(names, ", ")
		}
	}
	if len(unnamed) > 0 {
		what += "; the checks at " + strings.Join(unnamed, ", ") + " name their database other than by a literal"
	}
	return errors.New(what)
}
