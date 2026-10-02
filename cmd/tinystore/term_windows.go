package main

import (
	"os"
	"syscall"
)

var setConsoleMode = syscall.NewLazyDLL("kernel32.dll").NewProc("SetConsoleMode")

const enableVirtualTerminalProcessing = 0x0004

// enableColors asks the console to read the escapes that colour text
func enableColors(f *os.File) bool {
	handle := syscall.Handle(f.Fd())
	var mode uint32
	if err := syscall.GetConsoleMode(handle, &mode); err != nil {
		return false
	}
	if mode&enableVirtualTerminalProcessing != 0 {
		return true
	}
	//nolint:errcheck // the result says it: zero is a console that refuses, and the error only repeats why
	done, _, _ := setConsoleMode.Call(uintptr(handle), uintptr(mode|enableVirtualTerminalProcessing))
	return done != 0
}
