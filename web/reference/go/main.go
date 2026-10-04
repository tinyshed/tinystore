// Command reference prints a Go package's exported API as JSON for the
// site's API pages: each declaration's code without bodies, and its doc
// comment as markdown.
//
//	go run ./reference/go <package dir> <import path>
package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"go/ast"
	"go/doc"
	"go/doc/comment"
	"go/parser"
	"go/printer"
	"go/token"
	"io/fs"
	"os"
	"strings"
)

// Item is one declaration and the declarations under it: a type's
// constructors and methods. The shape is shared with the other languages'
// extractors in web/reference.
type Item struct {
	Name    string `json:"name"`
	Code    string `json:"code"`
	Doc     string `json:"doc"`
	Members []Item `json:"members,omitempty"`
}

type Package struct {
	Doc   string `json:"doc"`
	Items []Item `json:"items"`
}

func main() {
	if len(os.Args) != 3 {
		fmt.Fprintln(os.Stderr, "usage: reference <package dir> <import path>")
		os.Exit(2)
	}
	pkg, err := extract(os.Args[1], os.Args[2])
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	out := json.NewEncoder(os.Stdout)
	out.SetIndent("", "\t")
	if err := out.Encode(pkg); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func extract(dir, importPath string) (Package, error) {
	fset := token.NewFileSet()
	files, err := parseDir(fset, dir)
	if err != nil {
		return Package{}, err
	}
	p, err := doc.NewFromFiles(fset, files, importPath)
	if err != nil {
		return Package{}, fmt.Errorf("reading %s: %w", importPath, err)
	}

	w := writer{fset: fset, pkg: p}
	items := make([]Item, 0, len(p.Consts)+len(p.Vars)+len(p.Funcs)+len(p.Types))
	for _, v := range p.Consts {
		items = append(items, w.value(v))
	}
	for _, v := range p.Vars {
		items = append(items, w.value(v))
	}
	for _, f := range p.Funcs {
		items = append(items, w.function(f, ""))
	}
	for _, t := range p.Types {
		items = append(items, w.typ(t))
	}
	return Package{Doc: w.markdown(p.Doc), Items: items}, nil
}

func parseDir(fset *token.FileSet, dir string) ([]*ast.File, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}
	var files []*ast.File
	for _, e := range entries {
		if !isSource(e) {
			continue
		}
		f, err := parser.ParseFile(fset, dir+"/"+e.Name(), nil, parser.ParseComments)
		if err != nil {
			return nil, err
		}
		files = append(files, f)
	}
	return files, nil
}

func isSource(e fs.DirEntry) bool {
	name := e.Name()
	return !e.IsDir() && strings.HasSuffix(name, ".go") && !strings.HasSuffix(name, "_test.go")
}

type writer struct {
	fset *token.FileSet
	pkg  *doc.Package
}

// value names a const or var group after its first exported name.
func (w writer) value(v *doc.Value) Item {
	return Item{Name: v.Names[0], Code: w.code(v.Decl), Doc: w.markdown(v.Doc)}
}

func (w writer) function(f *doc.Func, typeName string) Item {
	decl := *f.Decl
	decl.Body = nil
	decl.Doc = nil
	name := f.Name
	if f.Recv != "" {
		name = typeName + "." + f.Name
	}
	return Item{Name: name, Code: w.code(&decl), Doc: w.markdown(f.Doc)}
}

func (w writer) typ(t *doc.Type) Item {
	decl := *t.Decl
	decl.Doc = nil
	item := Item{Name: t.Name, Code: w.code(&decl), Doc: w.markdown(t.Doc)}
	for _, v := range t.Consts {
		item.Members = append(item.Members, w.value(v))
	}
	for _, v := range t.Vars {
		item.Members = append(item.Members, w.value(v))
	}
	for _, f := range t.Funcs {
		item.Members = append(item.Members, w.function(f, t.Name))
	}
	for _, f := range t.Methods {
		item.Members = append(item.Members, w.function(f, t.Name))
	}
	return item
}

func (w writer) code(node any) string {
	var b bytes.Buffer
	config := printer.Config{Mode: printer.UseSpaces | printer.TabIndent, Tabwidth: 4}
	if err := config.Fprint(&b, w.fset, node); err != nil {
		return fmt.Sprintf("// %v", err)
	}
	return b.String()
}

// markdown turns a doc comment into markdown whose links to this package's
// names point at their headings on the same page, where the site checks them.
func (w writer) markdown(text string) string {
	printer := w.pkg.Printer()
	printer.HeadingLevel = 4
	printer.DocLinkURL = func(link *comment.DocLink) string {
		if link.ImportPath == "" || link.ImportPath == w.pkg.ImportPath {
			return "#" + strings.ToLower(link.Recv+link.Name)
		}
		return link.DefaultURL("https://pkg.go.dev")
	}
	return strings.TrimSpace(string(printer.Markdown(w.pkg.Parser().Parse(text))))
}
