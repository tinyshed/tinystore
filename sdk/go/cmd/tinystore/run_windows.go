package main

import (
	"errors"
	"fmt"
	"os"
	"os/exec"
	"os/signal"
)

// run starts the binary as a child, since Windows has no exec. The console's
// Ctrl+C reaches the child as well, so this process waits for it to finish
// rather than leave it behind holding its directory.
func run(binary string, args []string) int {
	signal.Ignore(os.Interrupt)
	child := exec.Command(binary, args...)
	child.Stdin, child.Stdout, child.Stderr = os.Stdin, os.Stdout, os.Stderr
	err := child.Run()
	var exited *exec.ExitError
	switch {
	case err == nil:
		return 0
	case errors.As(err, &exited):
		return exited.ExitCode()
	default:
		fmt.Fprintf(os.Stderr, "tinystore: cannot run %s: %v\n", binary, err)
		return 1
	}
}
