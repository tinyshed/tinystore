//go:build !windows

package term

import "os"

// enableColors has nothing to ask: a terminal here reads escape sequences.
func enableColors(*os.File) bool { return true }
