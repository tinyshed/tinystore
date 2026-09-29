//go:build linux || darwin || freebsd || dragonfly

package blobs

import "syscall"

// freeSpace is what the disk holding dir has free for an unprivileged process
func freeSpace(dir string) (int64, error) {
	var stat syscall.Statfs_t
	if err := syscall.Statfs(dir, &stat); err != nil {
		return 0, err
	}
	// The two counts are typed differently on each platform; as a float, bytes
	// near 2^62 lose nothing that matters.
	free := float64(stat.Bavail) * float64(stat.Bsize)
	return int64(min(free, 1<<62)), nil
}
