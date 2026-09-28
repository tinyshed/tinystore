package main

import (
	"os"
	"slices"
	"strings"
	"testing"
)

// what `go tool tinystore` adds to an application's module graph: the store
// it already requires, and the server that serve links
func TestTheToolRequiresOnlyTheStoreAndTheServer(t *testing.T) {
	text, err := os.ReadFile("go.mod")
	if err != nil {
		t.Fatal(err)
	}
	allowed := []string{"github.com/tinyshed/tinystore", "github.com/tinyshed/tinystore/server"}
	for _, module := range directRequirements(string(text)) {
		if !slices.Contains(allowed, module) {
			t.Errorf("go.mod requires %s; the tool requires the store and the server and nothing else", module)
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
