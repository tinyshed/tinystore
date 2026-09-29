// Command release builds into dist/ what a release publishes: the tinystore
// binary for every platform, an archive of each with SHA256SUMS, the npm
// packages that carry it, and the Python wheels that carry it.
//
//	go run ./internal/release -version v0.1.0
//	go run ./internal/release -version v0.1.0 -snapshot   from a working tree, to try it
//
// A release is built from a clean checkout of its tag, so that each binary's
// module version, which go build stamps from the checkout, is the tag. The
// SDKs' manifests must already name the same version: bumping them is a commit
// before the tag, not something this command does behind it.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"regexp"
)

// the release's version as a tag says it; the packages drop the v
var releaseVersion = regexp.MustCompile(`^v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$`)

type settings struct {
	tag      string // v0.1.0
	version  string // 0.1.0
	root     string // the repository
	out      string
	snapshot bool
}

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt)
	err := run(ctx)
	stop()
	if err != nil {
		fmt.Fprintln(os.Stderr, "release:", err)
		os.Exit(1)
	}
}

func run(ctx context.Context) error {
	s, err := parseSettings()
	if err != nil {
		return err
	}

	if err = checkManifests(s); err != nil {
		return err
	}
	if err = os.RemoveAll(s.out); err != nil {
		return err
	}

	binaries, err := buildBinaries(ctx, s)
	if err != nil {
		return err
	}
	if err = writeArchives(s, binaries); err != nil {
		return err
	}
	if err = writeNPMPackages(s, binaries); err != nil {
		return err
	}
	return writeWheels(ctx, s, binaries)
}

func parseSettings() (settings, error) {
	tag := flag.String("version", "", "the release's tag, such as v0.1.0")
	out := flag.String("out", "dist", "where to write, emptied first")
	snapshot := flag.Bool("snapshot", false, "build from the working tree, whatever version it stamps")
	flag.Parse()

	matched := releaseVersion.FindStringSubmatch(*tag)
	if matched == nil {
		return settings{}, fmt.Errorf("-version %q is not a tag such as v0.1.0", *tag)
	}
	root, err := os.Getwd()
	if err != nil {
		return settings{}, err
	}
	if _, err = os.Stat(filepath.Join(root, "cmd", "tinystore", "go.mod")); err != nil {
		return settings{}, errors.New("run it from the repository's root")
	}
	dir := *out
	if !filepath.IsAbs(dir) {
		dir = filepath.Join(root, dir)
	}
	return settings{tag: *tag, version: matched[1], root: root, out: dir, snapshot: *snapshot}, nil
}
