//go:build !(linux || darwin || freebsd || dragonfly || windows)

package blobs

import "errors"

// freeSpace does not know what is free on this system, so KeepFree checks nothing
func freeSpace(string) (int64, error) {
	return 0, errors.New("this system does not say what its disks have free")
}
