//go:build !(linux || darwin || dragonfly || freebsd || netbsd || openbsd || illumos || windows)

package dirlock

import "io"

// Supported is false where a second holder of the directory is not refused, as here.
const Supported = false

type none struct{}

func (none) Close() error { return nil }

// Hold holds nothing, on a platform without a lock its process's end releases.
func Hold(string) (io.Closer, error) { return none{}, nil }
