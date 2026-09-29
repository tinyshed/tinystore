// Package dirlock holds a store's directory for one process, through a lock on its LOCK file.
//
// The lock refuses a second holder in this process or another, and the
// operating system releases it when the process dies.
package dirlock

import "errors"

// Name is the file whose lock says a store holds the directory.
const Name = "LOCK"

// ErrHeld says another holder already holds the directory.
var ErrHeld = errors.New("the directory is held")
