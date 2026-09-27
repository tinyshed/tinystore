//go:build linux || darwin || freebsd || dragonfly

package blobs

import "syscall"

// freeSpace is what the disk holding dir has free for an unprivileged process
func freeSpace(dir string) (int64, error) {
	var stat syscall.Statfs_t
	if err := syscall.Statfs(dir, &stat); err != nil {
		return 0, err
	}
	return int64(min(uint64(stat.Bavail)*uint64(stat.Bsize), 1<<62)), nil //nolint:gosec,unconvert // the system's counts
}
