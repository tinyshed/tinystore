package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
)

// writeNotes writes dist/NOTES.md, the release's notes: the feat and fix
// subjects since the release before it, which AGENTS.md says the log is read
// for, and how to install this one in each language.
func writeNotes(ctx context.Context, s settings) error {
	tags, err := git(ctx, s.root, nil, "tag", "--list", "v*")
	if err != nil {
		return err
	}
	previous := previousRelease(s.tag, strings.Fields(tags))
	var subjects []string
	if previous != "" {
		end := s.tag
		if s.snapshot {
			end = "HEAD"
		}
		log, err := git(ctx, s.root, nil, "log", "--format=%s", previous+".."+end)
		if err != nil {
			return err
		}
		subjects = strings.Split(log, "\n")
	}
	notes := releaseNotes(s.tag, previous, subjects)
	return os.WriteFile(filepath.Join(s.out, "NOTES.md"), []byte(notes), 0o644)
}

// previousRelease is the release a release's notes begin after: the newest
// before it, a pre-release counting only before another pre-release, so that
// v0.2.0 says everything since v0.1.0 and not since v0.2.0-rc.2.
func previousRelease(tag string, tags []string) string {
	previous := ""
	for _, other := range tags {
		switch {
		case !releaseVersion.MatchString(other) || compareVersions(other, tag) >= 0:
		case isPreRelease(other) && !isPreRelease(tag):
		case previous == "" || compareVersions(other, previous) > 0:
			previous = other
		}
	}
	return previous
}

// a subject the changelog keeps: feat and fix, a scope, ! for a breaking change
var changelogSubject = regexp.MustCompile(`^(feat|fix)(?:\(([^)]+)\))?(!)?: (.+)$`)

// releaseNotes is the notes' text, a section a kind of change:
//
//	feat(kv): renew a sliding key once a refresh   →  ## Features
//	                                                  - **kv**: renew a sliding key once a refresh
//	fix!: refuse a label beginning __              →  ## Fixes
//	                                                  - **breaking**: refuse a label beginning __
func releaseNotes(tag, previous string, subjects []string) string {
	sections := map[string][]string{}
	for _, subject := range subjects {
		parts := changelogSubject.FindStringSubmatch(subject)
		if parts == nil {
			continue
		}
		line := parts[4]
		if parts[2] != "" {
			line = "**" + parts[2] + "**: " + line
		}
		if parts[3] != "" {
			line = "**breaking**: " + line
		}
		sections[parts[1]] = append(sections[parts[1]], "- "+line)
	}

	var notes strings.Builder
	if previous == "" {
		notes.WriteString("The first release.\n\n")
	}
	for _, section := range []struct{ kind, title string }{{"feat", "Features"}, {"fix", "Fixes"}} {
		if lines := sections[section.kind]; len(lines) > 0 {
			fmt.Fprintf(&notes, "## %s\n\n%s\n\n", section.title, strings.Join(lines, "\n"))
		}
	}
	version := strings.TrimPrefix(tag, "v")
	fmt.Fprintf(&notes, "## Install\n\n| | |\n|---|---|\n")
	fmt.Fprintf(&notes, "| Go | `go get %s@%s` |\n", module, tag)
	fmt.Fprintf(&notes, "| Bun | `bun add tinystore@%s` |\n", version)
	fmt.Fprintf(&notes, "| Python | `pip install tinyshed-tinystore==%s` |\n", pythonVersion(version))
	fmt.Fprintf(&notes, "| a server | `docker pull ghcr.io/tinyshed/tinystore:%s` |\n", version)
	fmt.Fprintf(&notes, "| the binary | an archive below, checked against `SHA256SUMS` |\n")
	if previous != "" {
		fmt.Fprintf(&notes, "\nEvery change: https://github.com/tinyshed/tinystore/compare/%s...%s\n", previous, tag)
	}
	return notes.String()
}
