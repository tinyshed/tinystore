package main

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
)

// the root module; the server's and the tool's paths begin with it
const module = "github.com/tinyshed/tinystore"

// tagRelease makes the three tags of a release in this repository, and
// nowhere else, and a worktree of the last for the build:
//
//	v0.1.0                 HEAD, the commit CI passed
//	server/v0.1.0          + server/go.mod requiring the store at v0.1.0
//	cmd/tinystore/v0.1.0   + cmd/tinystore/go.mod requiring both at v0.1.0
//
// The two commits after HEAD exist only under their tags, since main keeps the
// replace directives that a required module ignores and go install refuses.
// They take HEAD's dates, so that tagging the same HEAD again makes the same
// objects.
//
// go mod tidy reads the store and the server at their new tags from this
// repository through git, as the module proxy will read them from GitHub once
// they are pushed, so the go.sum lines it writes are the proxy's, and nothing
// leaves this machine before every tag is made and the release has built from
// them. It runs on a module cache of its own: one that once held another
// commit under the same version would hand that back.
func tagRelease(ctx context.Context, s settings) error {
	if err := checkTaggable(ctx, s); err != nil {
		return err
	}
	head, err := git(ctx, s.root, nil, "rev-parse", "HEAD")
	if err != nil {
		return err
	}
	date, err := git(ctx, s.root, nil, "show", "-s", "--format=%cI", "HEAD")
	if err != nil {
		return err
	}
	dated := []string{"GIT_AUTHOR_DATE=" + date, "GIT_COMMITTER_DATE=" + date}
	cache, err := os.MkdirTemp("", "tinystore-modcache-")
	if err != nil {
		return err
	}
	tidy := append(slices.Clone(s.goEnv), "GOMODCACHE="+cache)
	defer func() {
		if cleaned := goIn(ctx, s.root, tidy, "clean", "-modcache"); cleaned != nil {
			fmt.Fprintln(os.Stderr, "release: the module cache the tags were tidied in stays at", cache+":", cleaned)
		}
	}()

	if _, err = git(ctx, s.root, nil, "worktree", "add", "--detach", s.worktree, head); err != nil {
		return err
	}
	if _, err = git(ctx, s.root, dated, "tag", "-a", "-m", s.tag, s.tag, head); err != nil {
		return err
	}
	if err = requireRelease(ctx, s, tidy, dated, "server", "the store", module); err != nil {
		return err
	}
	return requireRelease(ctx, s, tidy, dated, "cmd/tinystore", "the store and the server", module, module+"/server")
}

// requireRelease makes a module of the worktree require others at the release
// without their replace directives, then commits and tags it.
func requireRelease(ctx context.Context, s settings, env, dated []string, dir, what string, required ...string) error {
	modules := filepath.Join(s.worktree, filepath.FromSlash(dir))
	edit := []string{"mod", "edit"}
	for _, path := range required {
		edit = append(edit, "-dropreplace="+path, "-require="+path+"@"+s.tag)
	}
	if err := goIn(ctx, modules, env, edit...); err != nil {
		return err
	}
	if err := goIn(ctx, modules, env, "mod", "tidy"); err != nil {
		return err
	}

	message := fmt.Sprintf("chore(release): require %s at %s", what, s.tag)
	commit := []string{"commit", "-q", "-m", message, "--", dir + "/go.mod", dir + "/go.sum"}
	if _, err := git(ctx, s.worktree, dated, commit...); err != nil {
		return err
	}
	_, err := git(ctx, s.worktree, dated, "tag", "-a", "-m", dir+"/"+s.tag, dir+"/"+s.tag, "HEAD")
	return err
}

// checkTaggable refuses a release that cannot be tagged here: changes not
// committed, a tag of the version already made, or a version that does not
// come after every release before it.
func checkTaggable(ctx context.Context, s settings) error {
	changed, err := git(ctx, s.root, nil, "status", "--porcelain", "--untracked-files=no")
	if err != nil {
		return err
	}
	if changed != "" {
		return fmt.Errorf("commit or set aside what changed before tagging:\n%s", changed)
	}
	tags, err := git(ctx, s.root, nil, "tag", "--list")
	if err != nil {
		return err
	}
	for tag := range strings.Lines(tags) {
		tag = strings.TrimSpace(tag)
		switch {
		case tag == s.tag || tag == "server/"+s.tag || tag == "cmd/tinystore/"+s.tag:
			return fmt.Errorf("%s is tagged already: a tag is never moved, and a mistake is the next version", tag)
		case releaseVersion.MatchString(tag) && compareVersions(tag, s.tag) >= 0:
			return fmt.Errorf("%s comes after %s, which would not be the newest release", tag, s.tag)
		}
	}
	return nil
}

// releaseEnv is the environment go builds a release in once its tags are made
// and before they are pushed: git finds github.com/tinyshed/tinystore in the
// repository at root, fetched directly and not checked against the checksum
// database, which knows no version before it is published.
func releaseEnv(ctx context.Context, root string) ([]string, error) {
	common, err := git(ctx, root, nil, "rev-parse", "--path-format=absolute", "--git-common-dir")
	if err != nil {
		return nil, err
	}
	return []string{
		"GOWORK=off",
		"GOPRIVATE=" + module,
		"GIT_CONFIG_COUNT=1",
		"GIT_CONFIG_KEY_0=url." + fileURL(common) + ".insteadOf",
		"GIT_CONFIG_VALUE_0=https://" + module,
	}, nil
}

// fileURL is a directory as git reads it in a URL:
//
//	D:\dev\tinystore\.git   →  file:///D:/dev/tinystore/.git
//	/home/me/tinystore/.git →  file:///home/me/tinystore/.git
func fileURL(dir string) string {
	slashed := filepath.ToSlash(dir)
	if !strings.HasPrefix(slashed, "/") {
		slashed = "/" + slashed
	}
	return "file://" + slashed
}

// git runs git in a directory and returns what it printed, trimmed.
func git(ctx context.Context, dir string, env []string, args ...string) (string, error) {
	command := exec.CommandContext(ctx, "git", args...)
	command.Dir = dir
	command.Env = append(os.Environ(), env...)
	var stderr bytes.Buffer
	command.Stderr = &stderr
	out, err := command.Output()
	if err != nil {
		return "", fmt.Errorf("git %s: %w\n%s", strings.Join(args, " "), err, stderr.Bytes())
	}
	return strings.TrimSpace(string(out)), nil
}

// goIn runs the go command in a directory.
func goIn(ctx context.Context, dir string, env []string, args ...string) error {
	command := exec.CommandContext(ctx, "go", args...)
	command.Dir = dir
	command.Env = append(os.Environ(), env...)
	if text, err := command.CombinedOutput(); err != nil {
		return fmt.Errorf("go %s in %s: %w\n%s", strings.Join(args, " "), dir, err, text)
	}
	return nil
}
