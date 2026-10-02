// Command release makes a TinyStore release: the three tags of its Go modules,
// and into dist/ the tinystore binary for every platform, an archive of each
// with SHA256SUMS, the npm packages and the Python wheels that carry it, and
// the release's notes.
//
//	go run ./internal/release -version v0.1.0 -tag           the tags, here only, and a worktree of the last
//	go -C <worktree> run ./internal/release -version v0.1.0  dist/, from that worktree
//	go run ./internal/release -version v0.1.0 -snapshot      dist/ from a working tree, to try it
//
// A release builds from a checkout of its cmd/tinystore tag, so that each
// binary's module version, which go build stamps from the checkout, is the
// tag. The SDKs' packages are stamped with the version as they are written:
// their manifests in the repository say 0.0.0, since a version lives in tags.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
)

type settings struct {
	tag      string // v0.1.0
	version  string // 0.1.0
	root     string // the repository
	out      string
	snapshot bool
	// tagOnly makes the release's tags and a worktree of the last at
	// worktree, and builds nothing.
	tagOnly  bool
	worktree string
	// goEnv is what go needs to read the store and the server at tags not
	// pushed yet; nothing for a snapshot.
	goEnv []string
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
	s, err := parseSettings(ctx)
	if err != nil {
		return err
	}
	if s.tagOnly {
		return tag(ctx, s)
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
	if err = buildJS(ctx, s); err != nil {
		return err
	}
	if err = writeNPMPackages(s, binaries); err != nil {
		return err
	}
	if err = writeWheels(ctx, s, binaries); err != nil {
		return err
	}
	return writeNotes(ctx, s)
}

// tag makes the release's tags and says what comes next, since nothing it
// made has left this repository.
func tag(ctx context.Context, s settings) error {
	if err := tagRelease(ctx, s); err != nil {
		return err
	}
	tags := s.tag + " server/" + s.tag + " cmd/tinystore/" + s.tag
	fmt.Printf("tagged %s here; nothing is pushed\n", tags)
	fmt.Printf("build:  go -C %s run ./internal/release -version %s\n", s.worktree, s.tag)
	fmt.Printf("push:   git push --atomic origin %s\n", tags)
	fmt.Printf("undo:   git tag -d %s && git worktree remove --force %s\n", tags, s.worktree)
	return nil
}

func parseSettings(ctx context.Context) (settings, error) {
	version := flag.String("version", "", "the release's tag, such as v0.1.0")
	out := flag.String("out", "dist", "where to write, emptied first")
	snapshot := flag.Bool("snapshot", false, "build from the working tree, whatever version it stamps")
	tagOnly := flag.Bool("tag", false, "make the release's tags here and a worktree of the last; build nothing")
	worktree := flag.String("worktree", "", "where -tag puts the worktree; a directory of the system's temporary one")
	flag.Parse()

	matched := releaseVersion.FindStringSubmatch(*version)
	if matched == nil || !publishable.MatchString(*version) {
		return settings{}, fmt.Errorf("-version %q is not a tag such as v0.1.0 or v0.1.0-rc.1", *version)
	}
	root, err := os.Getwd()
	if err != nil {
		return settings{}, err
	}
	if _, err = os.Stat(filepath.Join(root, "cmd", "tinystore", "go.mod")); err != nil {
		return settings{}, errors.New("run it from the repository's root")
	}
	s := settings{
		tag: *version, version: matched[1], root: root, out: absolute(root, *out),
		snapshot: *snapshot, tagOnly: *tagOnly, worktree: *worktree,
	}
	if s.worktree == "" {
		s.worktree = filepath.Join(os.TempDir(), "tinystore-"+s.tag)
	}
	if s.snapshot && s.tagOnly {
		return settings{}, errors.New("-snapshot builds without tags, and -tag makes them: one or the other")
	}
	if !s.snapshot {
		if s.goEnv, err = releaseEnv(ctx, root); err != nil {
			return settings{}, err
		}
	}
	return s, nil
}

func absolute(root, dir string) string {
	if filepath.IsAbs(dir) {
		return dir
	}
	return filepath.Join(root, dir)
}
