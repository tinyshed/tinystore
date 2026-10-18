package core

import (
	"strings"
	"syscall"
	"unsafe"
)

// On Windows the library is a DLL, called through syscall: no C compiler is
// needed to build a program that embeds the core.
type dllLibrary struct {
	openProc, sendProc, recvProc, closeProc *syscall.Proc
}

func loadLibrary(path string) (coreLibrary, error) {
	dll, err := syscall.LoadDLL(path)
	if err != nil {
		return nil, err
	}
	lib := &dllLibrary{}
	for name, proc := range map[string]**syscall.Proc{
		"tinystore_open": &lib.openProc, "tinystore_send": &lib.sendProc,
		"tinystore_recv": &lib.recvProc, "tinystore_close": &lib.closeProc,
	} {
		if *proc, err = dll.FindProc(name); err != nil {
			return nil, err
		}
	}
	return lib, nil
}

func (l *dllLibrary) open(dir, options []byte) (uintptr, error) {
	var conn uintptr
	failure := make([]byte, 4096)
	var optionsAt uintptr
	if len(options) > 0 {
		optionsAt = uintptr(unsafe.Pointer(&options[0]))
	}
	written, _, _ := l.openProc.Call(
		uintptr(unsafe.Pointer(&dir[0])), uintptr(len(dir)),
		optionsAt, uintptr(len(options)),
		0, 0,
		uintptr(unsafe.Pointer(&conn)),
		uintptr(unsafe.Pointer(&failure[0])), uintptr(len(failure)))
	if written > 0 {
		return 0, openFailure(string(dir), string(failure[:written]))
	}
	return conn, nil
}

func (l *dllLibrary) send(conn uintptr, frames []byte) {
	if len(frames) == 0 {
		return
	}
	_, _, _ = l.sendProc.Call(conn, uintptr(unsafe.Pointer(&frames[0])), uintptr(len(frames)), 0, 0)
}

func (l *dllLibrary) recv(conn uintptr, waitMs uint32, into []byte) int {
	n, _, _ := l.recvProc.Call(conn, uintptr(waitMs), uintptr(unsafe.Pointer(&into[0])), uintptr(len(into)))
	return int(n)
}

func (l *dllLibrary) close(conn uintptr) {
	_, _, _ = l.closeProc.Call(conn)
}

// openFailure is the error an open that failed names: in use when another
// process holds the directory, closed otherwise.
func openFailure(dir, message string) error {
	if strings.Contains(message, "in use") {
		return Errorf(InUse, "cannot open %s: %s", dir, message)
	}
	return Errorf(Closed, "cannot open %s: %s", dir, message)
}
