// Package dirlock holds a store's directory for one process: a lock on its
// LOCK file, which refuses a second holder in this process or another and
// which the operating system releases when the process dies.
package dirlock

import "errors"

// Name is the file whose lock says a store holds the directory.
const Name = "LOCK"

// ErrHeld is a directory another holder has.
var ErrHeld = errors.New("the directory is held")
