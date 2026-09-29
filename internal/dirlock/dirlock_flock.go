//go:build linux || darwin || dragonfly || freebsd || netbsd || openbsd || illumos

package dirlock

import (
	"errors"
	"io"
	"os"
	"path/filepath"
	"syscall"
)

const Supported = true

// Hold takes an exclusive flock of dir's LOCK, which the operating system
// also releases when the process dies; closing what it returns lets go.
func Hold(dir string) (io.Closer, error) {
	path := filepath.Join(dir, Name)
	file, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600) //nolint:gosec // the caller's own directory
	if err != nil {
		return nil, err
	}
	err = syscall.Flock(int(file.Fd()), syscall.LOCK_EX|syscall.LOCK_NB)
	if errors.Is(err, syscall.EWOULDBLOCK) {
		return nil, errors.Join(ErrHeld, file.Close())
	}
	if err != nil {
		return nil, errors.Join(err, file.Close())
	}
	return file, nil
}
