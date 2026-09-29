package sqldb

import (
	"regexp"
	"time"
)

const (
	readers      = 8               // four served 15 to 26 % fewer point reads in the container, as many on Windows
	statements   = 128             // compiled statements a connection keeps, about a megabyte of them
	writeSlots   = 2048            // writes at once: a group of 1024 gathering while one commits
	snapshotHold = 5 * time.Second // the longest an Each or a View holds its snapshot
	allBound     = 64 << 20        // bytes of decoded rows All holds
	firstHeld    = 64 << 10        // the store's memory a call reserves for its rows before it waits
)

// a name becomes a file name, so it stays short and plain
var validName = regexp.MustCompile(`^[a-z0-9][a-z0-9_-]{0,63}$`)
