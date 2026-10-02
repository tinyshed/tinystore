package main

import (
	"strings"
	"testing"
)

func TestNotesBeginAfterThePreviousReleaseOfTheirKind(t *testing.T) {
	tags := []string{"v0.1.0", "v0.1.1", "v0.2.0-rc.1", "v0.2.0-rc.2", "v0.3.0", "server/v0.1.1", "v0.x"}
	for tag, want := range map[string]string{
		"v0.1.0":      "",
		"v0.1.1":      "v0.1.0",
		"v0.2.0-rc.1": "v0.1.1",
		"v0.2.0-rc.2": "v0.2.0-rc.1",
		"v0.2.0":      "v0.1.1",
		"v0.2.1":      "v0.1.1",
	} {
		if got := previousRelease(tag, tags); got != want {
			t.Errorf("%s: begins after %q, want %q", tag, got, want)
		}
	}
}

func TestNotesKeepFeaturesAndFixesUnderTheirSections(t *testing.T) {
	notes := releaseNotes("v0.2.0-rc.1", "v0.1.0", []string{
		"feat(kv): renew a sliding key once a refresh",
		"chore(tools): bump golangci-lint to v2.14.0",
		"fix!: refuse a label beginning __",
		"feat: scan a page",
		"docs(sdk): show the API side by side",
	})
	for _, want := range []string{
		"## Features\n\n- **kv**: renew a sliding key once a refresh\n- scan a page\n\n",
		"## Fixes\n\n- **breaking**: refuse a label beginning __\n\n",
		"| Go | `go get github.com/tinyshed/tinystore@v0.2.0-rc.1` |",
		"| Bun, Node | `bun add tinystore@0.2.0-rc.1`, `npm install tinystore@0.2.0-rc.1` |",
		"| the command | `bunx tinystore@0.2.0-rc.1`, `uvx --from tinyshed-tinystore==0.2.0rc1 tinystore`, " +
			"`go install github.com/tinyshed/tinystore/cmd/tinystore@v0.2.0-rc.1` |",
		"| Python | `pip install tinyshed-tinystore==0.2.0rc1` |",
		"| a server | `docker pull ghcr.io/tinyshed/tinystore:0.2.0-rc.1` |",
		"compare/v0.1.0...v0.2.0-rc.1",
	} {
		if !strings.Contains(notes, want) {
			t.Errorf("the notes do not say %q:\n%s", want, notes)
		}
	}
	for _, unwanted := range []string{"golangci-lint", "side by side", "The first release"} {
		if strings.Contains(notes, unwanted) {
			t.Errorf("the notes say %q:\n%s", unwanted, notes)
		}
	}
}

func TestTheFirstReleaseSaysSoAndComparesWithNothing(t *testing.T) {
	notes := releaseNotes("v0.1.0", "", nil)
	if !strings.HasPrefix(notes, "The first release.\n\n## Install") || strings.Contains(notes, "compare/") {
		t.Errorf("the first release's notes:\n%s", notes)
	}
}
