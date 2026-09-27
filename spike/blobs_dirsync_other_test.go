//go:build !windows

package spike

import "os"

// blobsSyncDir makes a directory's names durable: its own fsync
func blobsSyncDir(path string) error {
	dir, err := os.Open(path)
	if err != nil {
		return err
	}
	return errorsJoin(dir.Sync(), dir.Close())
}
