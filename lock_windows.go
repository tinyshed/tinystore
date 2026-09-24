//go:build windows

package tinystore

import (
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"syscall"
)

const directoryLocking = true

// ERROR_SHARING_VIOLATION, which syscall does not name
const errorSharingViolation syscall.Errno = 32

// lockDirectory opens LOCK with no sharing, so every other open fails while
// this handle lives
func lockDirectory(dir string) (io.Closer, error) {
	name, err := syscall.UTF16PtrFromString(filepath.Join(dir, lockName))
	if err != nil {
		return nil, fmt.Errorf("lock %s: %w", dir, err)
	}
	handle, err := syscall.CreateFile(name, syscall.GENERIC_READ|syscall.GENERIC_WRITE, 0, nil,
		syscall.OPEN_ALWAYS, syscall.FILE_ATTRIBUTE_NORMAL, 0)
	if errors.Is(err, errorSharingViolation) {
		return nil, fmt.Errorf("%w: %s is open in another store", ErrInUse, dir)
	}
	if err != nil {
		return nil, fmt.Errorf("lock %s: %w", dir, err)
	}
	return os.NewFile(uintptr(handle), lockName), nil
}
