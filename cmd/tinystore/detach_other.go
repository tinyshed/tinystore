//go:build unix

package main

import (
	"os/exec"
	"syscall"
)

// detach starts a process that outlives this one, in a session of its own, so
// that the terminal that started it closing does not end it
func detach(child *exec.Cmd) {
	child.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
}
