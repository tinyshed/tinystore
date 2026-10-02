//go:build !unix && !windows

package main

import "os/exec"

// detach leaves a process as it starts where neither a session nor a process
// group can be its own
func detach(*exec.Cmd) {}
