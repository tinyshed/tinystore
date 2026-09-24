//go:build !(linux || darwin || dragonfly || freebsd || netbsd || openbsd || illumos || windows)

package tinystore

import "io"

const directoryLocking = false

type noLock struct{}

func (noLock) Close() error { return nil }

func lockDirectory(string) (io.Closer, error) { return noLock{}, nil }
