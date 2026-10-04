package tinystore

import (
	"os"
	"regexp"
	"strings"
	"testing"
)

// GATES.md names the test that keeps each promise, so a test renamed or
// deleted would leave its promise kept by nothing while the list still
// claims it.
func TestEveryGateNamesATestThatExists(t *testing.T) {
	gates, err := os.ReadFile("GATES.md")
	if err != nil {
		t.Fatal(err)
	}

	defined := definedTests(t)
	named := regexp.MustCompile("`((?:Test|Fuzz)[A-Za-z0-9_]+)`").FindAllStringSubmatch(string(gates), -1)
	if len(named) == 0 {
		t.Fatal("GATES.md names no test")
	}
	for _, name := range named {
		if !defined[name[1]] {
			t.Errorf("GATES.md names %s as a gate, and no module defines it", name[1])
		}
	}
}

// Claude Code reads a skill's description from .claude/skills, every other
// agent from .agents/skills, so a pointer whose description drifted sends one
// of them to the wrong skill or to none.
func TestEverySkillHasAPointerThatMatchesIt(t *testing.T) {
	for _, skill := range skillsIn(t, ".agents/skills") {
		pointer, err := os.ReadFile(".claude/skills/" + skill + "/SKILL.md")
		if err != nil || string(pointer) != pointerTo(t, skill) {
			t.Errorf(".claude/skills/%s/SKILL.md is not its skill's frontmatter and the line pointing to it", skill)
		}
	}
	for _, skill := range skillsIn(t, ".claude/skills") {
		if _, err := os.Stat(".agents/skills/" + skill + "/SKILL.md"); err != nil {
			t.Errorf(".claude/skills/%s points to no shared skill", skill)
		}
	}
}

func skillsIn(t *testing.T, dir string) []string {
	t.Helper()
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	names := make([]string, 0, len(entries))
	for _, entry := range entries {
		names = append(names, entry.Name())
	}
	return names
}

// pointerTo is what .claude/skills holds for a shared skill: its frontmatter,
// then the line that sends the reader to it.
func pointerTo(t *testing.T, skill string) string {
	t.Helper()
	text, err := os.ReadFile(".agents/skills/" + skill + "/SKILL.md")
	if err != nil {
		t.Fatal(err)
	}
	end := strings.Index(string(text), "\n---\n")
	if !strings.HasPrefix(string(text), "---\n") || end < 0 {
		t.Fatalf(".agents/skills/%s/SKILL.md begins with no frontmatter", skill)
	}
	return string(text[:end+5]) + "\nThe skill is shared with every agent working here: read `.agents/skills/" +
		skill + "/SKILL.md`\nand follow it. Change it there, never here.\n"
}

// definedTests is every test and fuzz target of every module in the checkout.
func definedTests(t *testing.T) map[string]bool {
	t.Helper()

	declared := regexp.MustCompile(`(?m)^func ((?:Test|Fuzz)[A-Za-z0-9_]+)\(`)
	defined := map[string]bool{}
	for _, file := range goFiles(t) {
		if !strings.HasSuffix(file, "_test.go") {
			continue
		}
		text, err := os.ReadFile(file)
		if err != nil {
			t.Fatal(err)
		}
		for _, match := range declared.FindAllSubmatch(text, -1) {
			defined[string(match[1])] = true
		}
	}
	return defined
}
