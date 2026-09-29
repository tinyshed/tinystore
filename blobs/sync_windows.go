//go:build windows

package blobs

import (
	"errors"
	"sync"
	"syscall"
	"unsafe"
)

// simultaneous directory walks contend inside Windows while opening and closing their handles
type nameCalls struct{ sync.Mutex }

// ERROR_SHARING_VIOLATION, which syscall does not name
const errorSharingViolation syscall.Errno = 32

// syncDirectory makes a directory's names durable. Go's Sync of a directory is
// refused on Windows, so it is opened for writing with backup semantics and
// flushed, which takes about a millisecond.
func syncDirectory(path string) error {
	name, err := syscall.UTF16PtrFromString(path)
	if err != nil {
		return err
	}
	const share = syscall.FILE_SHARE_READ | syscall.FILE_SHARE_WRITE | syscall.FILE_SHARE_DELETE
	handle, err := syscall.CreateFile(name, syscall.GENERIC_WRITE, share, nil, syscall.OPEN_EXISTING,
		syscall.FILE_FLAG_BACKUP_SEMANTICS, 0)
	if err != nil {
		return err
	}
	return errors.Join(syscall.FlushFileBuffers(handle), syscall.CloseHandle(handle))
}

// heldElsewhere says a rename failed because another program, a scanner or a
// backup tool, opened the file without sharing it
func heldElsewhere(err error) bool {
	return errors.Is(err, syscall.ERROR_ACCESS_DENIED) || errors.Is(err, errorSharingViolation)
}

// beingRemoved says an open failed on a file whose removal has begun. Until the
// handle that removes it closes, Windows refuses to open it rather than saying
// it is gone.
func beingRemoved(err error) bool {
	return errors.Is(err, syscall.ERROR_ACCESS_DENIED)
}

var getDiskFreeSpaceEx = syscall.NewLazyDLL("kernel32.dll").NewProc("GetDiskFreeSpaceExW")

// freeSpace is what the disk holding dir has free for this process
func freeSpace(dir string) (int64, error) {
	name, err := syscall.UTF16PtrFromString(dir)
	if err != nil {
		return 0, err
	}
	// The count the system fills is unsigned, and no disk takes it past 2⁶³.
	// The pointers are made inside the call, so that the stack cannot move
	// under them.
	var free int64
	//nolint:gosec // the call's own parameters
	succeeded, _, failure := getDiskFreeSpaceEx.Call(uintptr(unsafe.Pointer(name)), uintptr(unsafe.Pointer(&free)),
		0, 0)
	if succeeded == 0 {
		return 0, failure
	}
	return free, nil
}
