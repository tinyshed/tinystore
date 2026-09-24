package tinystore

import "errors"

// Every engine wraps these, so errors.Is means the same in all of them.
var (
	ErrInvalid   = errors.New("invalid request")
	ErrLimit     = errors.New("resource limit")
	ErrClosed    = errors.New("store is closed")
	ErrInUse     = errors.New("already in use")
	ErrConflict  = errors.New("state changed")
	ErrCorrupt   = errors.New("corrupt data")
	ErrTooOld    = errors.New("too old")
	ErrSuspended = errors.New("suspended")
)
