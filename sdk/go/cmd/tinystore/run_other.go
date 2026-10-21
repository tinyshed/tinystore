//go:build !windows

package main

import (
	"fmt"
	"os"
	"syscall"
)

// run replaces this process with the binary, so that signals, stdin and the
// exit code are the binary's own.
func run(binary string, args []string) int {
	err := syscall.Exec(binary, append([]string{binary}, args...), os.Environ())
	fmt.Fprintf(os.Stderr, "tinystore: cannot run %s: %v\n", binary, err)
	return 1
}
