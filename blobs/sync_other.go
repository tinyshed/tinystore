//go:build !windows

package blobs

import (
	"errors"
	"os"
)

// syncDirectory makes a directory's names durable: its own fsync
func syncDirectory(path string) error {
	dir, err := os.Open(path) //nolint:gosec // a directory of the store's own
	if err != nil {
		return err
	}
	return errors.Join(dir.Sync(), dir.Close())
}

// heldElsewhere is false where a rename does not ask what else holds the file
func heldElsewhere(error) bool {
	return false
}

// beingRemoved is false where a removed name is gone at once
func beingRemoved(error) bool {
	return false
}
