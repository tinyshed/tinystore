package main

import (
	"runtime"
	"testing"
)

func TestARepositoryIsAFileURLGitReads(t *testing.T) {
	dir, want := "/home/me/tinystore/.git", "file:///home/me/tinystore/.git"
	if runtime.GOOS == "windows" {
		dir, want = `D:\dev\tinystore\.git`, "file:///D:/dev/tinystore/.git"
	}
	if got := fileURL(dir); got != want {
		t.Errorf("%s: %s, want %s", dir, got, want)
	}
}
