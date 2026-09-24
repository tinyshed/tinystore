//go:build linux || darwin || dragonfly || freebsd || netbsd || openbsd || illumos

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

// lockDirectory takes an exclusive flock, which the operating system also
// releases when the process dies
func lockDirectory(dir string) (io.Closer, error) {
	path := filepath.Join(dir, lockName)
	file, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600) //nolint:gosec // the caller's own directory
	if err != nil {
		return nil, fmt.Errorf("lock %s: %w", dir, err)
	}
	err = syscall.Flock(int(file.Fd()), syscall.LOCK_EX|syscall.LOCK_NB)
	if errors.Is(err, syscall.EWOULDBLOCK) {
		return nil, errors.Join(fmt.Errorf("%w: %s is open in another store", ErrInUse, dir), file.Close())
	}
	if err != nil {
		return nil, errors.Join(fmt.Errorf("lock %s: %w", dir, err), file.Close())
	}
	return file, nil
}
