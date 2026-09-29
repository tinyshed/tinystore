package main

import (
	"encoding/json"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"strings"
)

// the scope of the packages that carry the binary, one a platform, as the
// SDK's packagedBinary looks for them
const binaryScope = "@tinyshed/tinystore-"

// checkManifests refuses a release whose SDKs' manifests name another version.
func checkManifests(s settings) error {
	pkg, err := readJSON(filepath.Join(s.root, "sdk", "js", "package.json"))
	if err != nil {
		return err
	}
	if pkg["version"] != s.version {
		return fmt.Errorf("sdk/js/package.json is %v, not %s", pkg["version"], s.version)
	}
	pyproject, err := os.ReadFile(filepath.Join(s.root, "sdk", "python", "pyproject.toml"))
	if err != nil {
		return err
	}
	named := regexp.MustCompile(`(?m)^version = "([^"]+)"`).FindSubmatch(pyproject)
	if named == nil || string(named[1]) != pythonVersion(s.version) {
		return fmt.Errorf("sdk/python/pyproject.toml does not say version = %q", pythonVersion(s.version))
	}
	return nil
}

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

// writeSDKPackage publishes sdk/js as it is, its version checked, with the
// platforms' packages as optional dependencies and nothing only its
// development needs.
func writeSDKPackage(s settings, dir string, optional map[string]string) error {
	source := filepath.Join(s.root, "sdk", "js")
	manifest, err := readJSON(filepath.Join(source, "package.json"))
	if err != nil {
		return err
	}
	delete(manifest, "devDependencies")
	delete(manifest, "scripts")
	manifest["optionalDependencies"] = optional

	files := map[string]string{"LICENSE": filepath.Join(s.root, "LICENSE")}
	err = filepath.WalkDir(filepath.Join(source, "src"), func(path string, entry fs.DirEntry, err error) error {
		if err != nil || entry.IsDir() {
			return err
		}
		relative, err := filepath.Rel(source, path)
		files[filepath.ToSlash(relative)] = path
		return err
	})
	if err != nil {
		return err
	}
	if _, err = os.Stat(filepath.Join(source, "README.md")); err == nil {
		files["README.md"] = filepath.Join(source, "README.md")
	}
	name, ok := manifest["name"].(string)
	if !ok {
		return fmt.Errorf("sdk/js/package.json names no package")
	}
	return writePackage(filepath.Join(dir, tarballName(name, s.version)), manifest, files)
}

// writePackage writes a tarball as npm pack does: every file under package/,
// the manifest as package/package.json.
func writePackage(path string, manifest map[string]any, files map[string]string) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	body, err := json.MarshalIndent(manifest, "", "  ")
	if err != nil {
		return err
	}
	staged := filepath.Join(filepath.Dir(path), ".package.json")
	if err = os.WriteFile(staged, append(body, '\n'), 0o644); err != nil {
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

func readJSON(path string) (map[string]any, error) {
	body, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var parsed map[string]any
	if err = json.Unmarshal(body, &parsed); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	return parsed, nil
}
