package main

import (
	"errors"
	"os"
	"os/exec"
	"testing"
)

// The command replaces its process on Unix, so it runs in a child of its own;
// this test's binary stands in for tinystore, and its exit code proves the
// arguments arrived.
func TestMain(m *testing.M) {
	if len(os.Args) > 1 && os.Args[1] == "exit-with-7" {
		os.Exit(7)
	}
	if os.Getenv("TINYSTORE_COMMAND_TEST") == "run" {
		os.Exit(run(os.Args[0], []string{"exit-with-7"}))
	}
	os.Exit(m.Run())
}

func TestTheCommandRunsTheReleasesBinaryWithItsArguments(t *testing.T) {
	command := exec.Command(os.Args[0])
	command.Env = append(os.Environ(), "TINYSTORE_COMMAND_TEST=run")
	var exited *exec.ExitError
	if err := command.Run(); !errors.As(err, &exited) || exited.ExitCode() != 7 {
		t.Fatalf("the binary's exit is the command's: %v", err)
	}
}
