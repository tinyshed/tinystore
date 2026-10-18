package tinystore

import (
	"errors"
	"io/fs"
	"os"
	"regexp"
	"runtime"
	"strings"
	"unicode"

	"github.com/tinyshed/tinystore/internal/core"
)

// The core parses every file and reads every variable, so that every
// language reads YAML and a variable alike; what the program reads itself is
// what belongs to it and not to the server: a file's text, its variables, the
// files their NAME_FILE names and its .env files.

// layersData is the layers as the core reads them, in the order written.
func layersData(d *declared, layers []layer) ([]any, error) {
	paths := settingPaths(d.root, "")
	out := make([]any, 0, len(layers))
	for _, each := range layers {
		data, err := each.data(paths)
		if err != nil {
			return nil, err
		}
		out = append(out, data)
	}
	return out, nil
}

// settingPaths is every setting's path, as the core names its variable.
func settingPaths(group *setting, path string) []string {
	var found []string
	for _, node := range group.group {
		at := joinPath(path, node.key)
		if node.kind == kindGroup {
			found = append(found, settingPaths(node, at)...)
			continue
		}
		found = append(found, at)
	}
	return found
}

func (f fileLayer) data([]string) (map[string]any, error) {
	file := map[string]any{"name": f.path, "optional": f.optional}
	text, found, err := readText(f.path)
	if err != nil {
		return nil, err
	}
	if found {
		file["text"] = text
	}
	return map[string]any{"file": file}, nil
}

// readText is a file's text, and whether it exists.
func readText(path string) (string, bool, error) {
	text, err := os.ReadFile(path)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return "", false, nil
	case err != nil:
		return "", false, core.Wrap(core.Invalid, err, "%s", path)
	}
	return string(text), true, nil
}

// data is what the environment gives, read here: the variables of every
// setting and those with the prefix, never the rest of the process's, the
// files their NAME_FILE names, and the .env files' text.
func (e envLayer) data(paths []string) (map[string]any, error) {
	key := func(name string) string {
		if runtime.GOOS == "windows" {
			return strings.ToUpper(name)
		}
		return name
	}
	wanted := map[string]bool{}
	for _, path := range paths {
		name := e.vars[path]
		if name == "" {
			name = envName(e.prefix, path)
		}
		wanted[key(name)], wanted[key(name+"_FILE")] = true, true
	}
	start := key(e.prefix + "_")
	variables := map[string]string{}
	for name, value := range e.environment() {
		k := key(name)
		if wanted[k] || e.prefix != "" && strings.HasPrefix(k, start) {
			variables[k] = value
		}
	}
	dotenv := []map[string]string{}
	for _, name := range e.files {
		text, found, err := readText(name)
		if err != nil {
			return nil, err
		}
		if found {
			dotenv = append(dotenv, map[string]string{"name": name, "text": text})
		}
	}
	files, err := secretFiles(variables, dotenv)
	if err != nil {
		return nil, err
	}
	return map[string]any{"env": map[string]any{
		"prefix": e.prefix, "vars": e.vars, "variables": variables, "files": files, "dotenv": dotenv,
	}}, nil
}

// environment is the variables the layer reads: its own, or the process's.
func (e envLayer) environment() map[string]string {
	if e.from != nil {
		return e.from
	}
	out := map[string]string{}
	for _, pair := range os.Environ() {
		if name, value, ok := strings.Cut(pair, "="); ok && name != "" {
			out[name] = value
		}
	}
	return out
}

// secretFiles is the text of each file a NAME_FILE variable names, in the
// process's variables or a .env file's, which the core cannot read.
func secretFiles(variables map[string]string, dotenv []map[string]string) (map[string]string, error) {
	files := map[string]string{}
	read := func(name, path string) error {
		if !strings.HasSuffix(name, "_FILE") {
			return nil
		}
		text, found, err := readText(path)
		if found {
			files[path] = text
		}
		return err
	}
	for name, path := range variables {
		if err := read(name, path); err != nil {
			return nil, err
		}
	}
	for _, file := range dotenv {
		for _, line := range strings.Split(file["text"], "\n") {
			found := fileLine.FindStringSubmatch(line)
			if found == nil {
				continue
			}
			if err := read(found[1], unquoted(found[2])); err != nil {
				return nil, err
			}
		}
	}
	return files, nil
}

var fileLine = regexp.MustCompile(`^\s*(?:export\s+)?([\p{L}_][\p{L}\p{N}_]*_FILE)\s*=\s*(.*?)\s*$`)

func unquoted(text string) string {
	if len(text) >= 2 && (text[0] == '"' || text[0] == '\'') && text[len(text)-1] == text[0] {
		return text[1 : len(text)-1]
	}
	return text
}

// envName is a setting's variable: the prefix and its path in upper snake
// case, as every language names it (testdata/config/env.json).
//
//	envName("APP", "limits.maxRps")  is  APP_LIMITS_MAX_RPS
func envName(prefix, path string) string {
	runes := []rune(path)
	var name strings.Builder
	for i, c := range runes {
		if c == '.' || c == '-' {
			name.WriteByte('_')
			continue
		}
		if i > 0 && unicode.IsUpper(c) {
			before := runes[i-1]
			lowerAfter := i+1 < len(runes) && unicode.IsLower(runes[i+1])
			if unicode.IsLower(before) || unicode.IsDigit(before) || unicode.IsUpper(before) && lowerAfter {
				name.WriteByte('_')
			}
		}
		name.WriteString(strings.ToUpper(string(c)))
	}
	if prefix == "" {
		return name.String()
	}
	return prefix + "_" + name.String()
}
