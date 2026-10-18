//go:build !windows

package core

import (
	"os/exec"
	"syscall"
)

func hideWindow(*exec.Cmd) {}

// detach makes a process outlive this one, in a session of its own.
func detach(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
}
