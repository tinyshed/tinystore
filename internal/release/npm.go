package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
)

// the scope of the packages that carry the binary, one a platform, as the
// SDK's packagedBinary looks for them
const binaryScope = "@tinyshed/tinystore-"

// pythonVersion spells a release's version as PyPI keeps it, which npm and Go
// spell with a hyphen and dots:
//
//	0.1.0         →  0.1.0
//	0.1.0-rc.1    →  0.1.0rc1
//	0.1.0-beta.2  →  0.1.0b2
//	0.1.0-alpha.3 →  0.1.0a3
func pythonVersion(version string) string {
	release, pre, found := strings.Cut(version, "-")
	if !found {
		return version
	}
	kind, number, _ := strings.Cut(pre, ".")
	short := map[string]string{"alpha": "a", "beta": "b", "rc": "rc"}[kind]
	if short == "" {
		return version // not a pre-release PyPI knows; the check says so
	}
	return release + short + number
}

// buildJS compiles sdk/js into its dist, which its files names: the
// JavaScript and the declarations Node imports, since Node strips no types in
// a package it installed, while Bun imports the TypeScript itself.
func buildJS(ctx context.Context, s settings) error {
	source := filepath.Join(s.root, "sdk", "js")
	if err := os.RemoveAll(filepath.Join(source, "dist")); err != nil {
		return err
	}
	for _, args := range [][]string{{"install", "--frozen-lockfile"}, {"run", "build"}} {
		bun := exec.CommandContext(ctx, "bun", args...)
		bun.Dir = source
		if text, err := bun.CombinedOutput(); err != nil {
			return fmt.Errorf("bun %s in sdk/js: %w\n%s", strings.Join(args, " "), err, text)
		}
	}
	return nil
}

// writeNPMPackages writes the package of each platform's binary and the SDK's
// package that names them all as optional dependencies, each a tarball npm
// publish takes: npm installs only the one whose os and cpu match.
//
// The tarballs are written here rather than by npm pack, which on Windows
// packs every file without its executable bit.
func writeNPMPackages(s settings, binaries []binary) error {
	dir := filepath.Join(s.out, "npm")
	optional := map[string]string{}
	for _, b := range binaries {
		name := binaryScope + b.npmOS + "-" + b.npmCPU
		optional[name] = s.version
		if err := writeBinaryPackage(s, dir, name, b); err != nil {
			return err
		}
	}
	return writeSDKPackage(s, dir, optional)
}

func writeBinaryPackage(s settings, dir, name string, b binary) error {
	manifest := map[string]any{
		"name":        name,
		"version":     s.version,
		"description": fmt.Sprintf("The tinystore binary for %s/%s, run by the tinystore package", b.goos, b.goarch),
		"license":     "Apache-2.0",
		"repository":  repository("cmd/tinystore"),
		"os":          []string{b.npmOS},
		"cpu":         []string{b.npmCPU},
		"files":       []string{"bin"},
	}
	files := map[string]string{
		"bin/" + b.executable(): b.path,
		"LICENSE":               filepath.Join(s.root, "LICENSE"),
	}
	return writePackage(filepath.Join(dir, tarballName(name, s.version)), manifest, files)
}

// writeSDKPackage publishes sdk/js as it is, stamped with the release's
// version, with the platforms' packages as optional dependencies and nothing
// only its development needs: the files its manifest's files names, as npm
// pack takes them.
func writeSDKPackage(s settings, dir string, optional map[string]string) error {
	source := filepath.Join(s.root, "sdk", "js")
	manifest, err := readManifest(filepath.Join(source, "package.json"))
	if err != nil {
		return err
	}
	manifest.remove("devDependencies")
	manifest.remove("scripts")
	err = errors.Join(manifest.set("version", s.version), manifest.set("optionalDependencies", optional))
	if err != nil {
		return err
	}

	files := map[string]string{"LICENSE": filepath.Join(s.root, "LICENSE")}
	var listed []string
	if err = manifest.get("files", &listed); err != nil {
		return fmt.Errorf("sdk/js/package.json's files, without which npm would pack the whole directory: %w", err)
	}
	for _, path := range listed {
		if err = addFiles(files, source, path); err != nil {
			return err
		}
	}
	var name string
	if err = manifest.get("name", &name); err != nil {
		return fmt.Errorf("sdk/js/package.json's name: %w", err)
	}
	return writePackage(filepath.Join(dir, tarballName(name, s.version)), manifest, files)
}

// manifest is a package.json whose keys keep their order, since npm reads
// some of them in order: the conditions of exports are tried one after
// another, the first that matches winning, so default stays last.
type manifest struct {
	keys   []string
	fields map[string]json.RawMessage
}

func readManifest(path string) (*manifest, error) {
	text, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	decoder := json.NewDecoder(bytes.NewReader(text))
	open, err := decoder.Token()
	if err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	if open != json.Delim('{') {
		return nil, fmt.Errorf("%s is not a JSON object", path)
	}
	m := &manifest{fields: map[string]json.RawMessage{}}
	for decoder.More() {
		token, err := decoder.Token()
		if err != nil {
			return nil, fmt.Errorf("%s: %w", path, err)
		}
		key, _ := token.(string) //nolint:errcheck // an object's member begins with its name, a string
		var value json.RawMessage
		if err = decoder.Decode(&value); err != nil {
			return nil, fmt.Errorf("%s: %s: %w", path, key, err)
		}
		if _, seen := m.fields[key]; !seen {
			m.keys = append(m.keys, key)
		}
		m.fields[key] = value
	}
	return m, nil
}

func (m *manifest) get(key string, value any) error {
	raw, ok := m.fields[key]
	if !ok {
		return fmt.Errorf("no %s", key)
	}
	return json.Unmarshal(raw, value)
}

// set replaces a field where it stands, or adds it after the others
func (m *manifest) set(key string, value any) error {
	raw, err := json.Marshal(value)
	if err != nil {
		return err
	}
	if _, ok := m.fields[key]; !ok {
		m.keys = append(m.keys, key)
	}
	m.fields[key] = raw
	return nil
}

func (m *manifest) remove(key string) {
	delete(m.fields, key)
	m.keys = slices.DeleteFunc(m.keys, func(k string) bool { return k == key })
}

func (m *manifest) MarshalJSON() ([]byte, error) {
	object := []byte{'{'}
	for i, key := range m.keys {
		if i > 0 {
			object = append(object, ',')
		}
		name, err := json.Marshal(key)
		if err != nil {
			return nil, err
		}
		object = append(append(append(object, name...), ':'), m.fields[key]...)
	}
	return append(object, '}'), nil
}

// addFiles adds the file a package's files names, or every file under the
// directory it names, by its path in the package
func addFiles(files map[string]string, source, listed string) error {
	return filepath.WalkDir(filepath.Join(source, filepath.FromSlash(listed)),
		func(path string, entry fs.DirEntry, err error) error {
			if err != nil {
				return fmt.Errorf("sdk/js/package.json's files names %s: %w", listed, err)
			}
			if entry.IsDir() {
				return nil
			}
			relative, err := filepath.Rel(source, path)
			files[filepath.ToSlash(relative)] = path
			return err
		})
}

// writePackage writes a tarball as npm pack does: every file under package/,
// the manifest as package/package.json, its >= left as it is spelled.
func writePackage(path string, manifest any, files map[string]string) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	var body bytes.Buffer
	encoder := json.NewEncoder(&body)
	encoder.SetEscapeHTML(false)
	encoder.SetIndent("", "  ")
	if err := encoder.Encode(manifest); err != nil {
		return err
	}
	staged := filepath.Join(filepath.Dir(path), ".package.json")
	if err := os.WriteFile(staged, body.Bytes(), 0o644); err != nil {
		return err
	}
	defer func() { _ = os.Remove(staged) }() // written again by the next package
	entries := map[string]string{"package/package.json": staged}
	for name, from := range files {
		entries["package/"+name] = from
	}
	return writeTarGz(path, entries)
}

// @tinyshed/tinystore-linux-x64 at 0.1.0 → tinyshed-tinystore-linux-x64-0.1.0.tgz
func tarballName(name, version string) string {
	return regexp.MustCompile(`^@|/`).ReplaceAllStringFunc(name, func(m string) string {
		if m == "/" {
			return "-"
		}
		return ""
	}) + "-" + version + ".tgz"
}

func repository(directory string) map[string]string {
	return map[string]string{
		"type":      "git",
		"url":       "git+https://github.com/tinyshed/tinystore.git",
		"directory": directory,
	}
}
