//go:build unix

package private

import (
	"fmt"
	"os"
	"syscall"
)

// restrict refuses a directory another user owns, whose mode it could not
// make its own, and sets 0700
func restrict(dir string, found os.FileInfo) error {
	if stat, ok := found.Sys().(*syscall.Stat_t); ok && int(stat.Uid) != os.Getuid() {
		return fmt.Errorf("%w: owned by user %d", errNotADirectory, stat.Uid)
	}
	return os.Chmod(dir, 0o700) //nolint:gosec // a directory, which its owner must be able to enter
}

// Check says whether dir is its owner's alone.
func Check(dir string) error {
	found, err := os.Lstat(dir)
	if err != nil {
		return err
	}
	if mode := found.Mode(); !mode.IsDir() || mode.Perm() != 0o700 {
		return fmt.Errorf("%s is %s", dir, mode)
	}
	return nil
}
