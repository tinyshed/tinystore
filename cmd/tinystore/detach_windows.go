package main

import (
	"os/exec"
	"syscall"
)

const (
	detachedProcess = 0x00000008
	createNoWindow  = 0x08000000
)

// detach starts a process that outlives this one, in no console and a group
// of its own, so that a Ctrl+C in the terminal that started it does not end it
func detach(child *exec.Cmd) {
	child.SysProcAttr = &syscall.SysProcAttr{
		CreationFlags: detachedProcess | createNoWindow | syscall.CREATE_NEW_PROCESS_GROUP,
		HideWindow:    true,
	}
}
