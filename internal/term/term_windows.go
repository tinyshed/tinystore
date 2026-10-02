package term

import (
	"os"
	"syscall"
)

var setConsoleMode = syscall.NewLazyDLL("kernel32.dll").NewProc("SetConsoleMode")

const enableVirtualTerminalProcessing = 0x0004

// enableColors turns on the console's reading of escape sequences, which stays
// on for the console after this process; it says false where the console
// refuses, as one older than Windows 10 does.
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
