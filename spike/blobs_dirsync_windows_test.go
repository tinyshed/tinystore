//go:build windows

package spike

import "syscall"

// blobsSyncDir makes a directory's names durable on Windows, where Go's Sync of
// a directory is refused: the directory opened for writing with backup
// semantics, then flushed
func blobsSyncDir(path string) error {
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
	return errorsJoin(syscall.FlushFileBuffers(handle), syscall.CloseHandle(handle))
}
