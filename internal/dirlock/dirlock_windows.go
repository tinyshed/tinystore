//go:build windows

package dirlock

import (
	"errors"
	"io"
	"os"
	"path/filepath"
	"syscall"
)

const Supported = true

// ERROR_SHARING_VIOLATION, which syscall does not name
const errorSharingViolation syscall.Errno = 32

// Hold opens dir's LOCK with no sharing, so that every other open fails while
// what it returns is open.
func Hold(dir string) (io.Closer, error) {
	name, err := syscall.UTF16PtrFromString(filepath.Join(dir, Name))
	if err != nil {
		return nil, err
	}
	handle, err := syscall.CreateFile(name, syscall.GENERIC_READ|syscall.GENERIC_WRITE, 0, nil,
		syscall.OPEN_ALWAYS, syscall.FILE_ATTRIBUTE_NORMAL, 0)
	if errors.Is(err, errorSharingViolation) {
		return nil, ErrHeld
	}
	if err != nil {
		return nil, err
	}
	return os.NewFile(uintptr(handle), Name), nil
}
