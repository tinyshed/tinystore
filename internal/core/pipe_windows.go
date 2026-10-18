package core

import (
	"context"
	"errors"
	"io"
	"os"
	"syscall"
	"time"
)

// A Windows named pipe is opened overlapped and given to os.NewFile, as Go's
// runtime polls it: a synchronous handle would hold every write behind a
// pending read.

// the errors and flags syscall does not name
const (
	errorPipeBusy          = syscall.Errno(231)
	securitySQOSPresent    = 0x00100000
	securityIdentification = 0x00010000
)

func dialPipe(ctx context.Context, name string) (io.ReadWriteCloser, error) {
	full := `\\.\pipe\` + name
	path, err := syscall.UTF16PtrFromString(full)
	if err != nil {
		return nil, err
	}
	for {
		handle, err := syscall.CreateFile(path, syscall.GENERIC_READ|syscall.GENERIC_WRITE, 0, nil,
			syscall.OPEN_EXISTING, syscall.FILE_FLAG_OVERLAPPED|securitySQOSPresent|securityIdentification, 0)
		if err == nil {
			return os.NewFile(uintptr(handle), full), nil
		}
		if !errors.Is(err, errorPipeBusy) {
			return nil, err
		}
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-time.After(5 * time.Millisecond):
		}
	}
}
