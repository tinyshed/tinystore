package core

import (
	"os/exec"
	"syscall"
)

const (
	detachedProcess = 0x00000008
	createNoWindow  = 0x08000000
)

// hideWindow keeps a child from opening a console of its own.
func hideWindow(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{HideWindow: true, CreationFlags: createNoWindow}
}

// detach makes a process outlive this one, with no console of its own.
func detach(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{
		HideWindow:    true,
		CreationFlags: detachedProcess | syscall.CREATE_NEW_PROCESS_GROUP | createNoWindow,
	}
}
