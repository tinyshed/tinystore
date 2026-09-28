//go:build !unix && !windows

package private

import "os"

func restrict(dir string, _ os.FileInfo) error {
	return os.Chmod(dir, 0o700)
}

// Check says whether dir is its owner's alone, which nothing here can tell.
func Check(string) error { return nil }
