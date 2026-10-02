package tinystore

import (
	"errors"
	"fmt"
)

// Every engine wraps these, so errors.Is means the same in all of them.
var (
	ErrInvalid   = errors.New("invalid request")
	ErrLimit     = errors.New("resource limit")
	ErrClosed    = errors.New("store is closed")
	ErrInUse     = errors.New("already in use")
	ErrConflict  = errors.New("state changed")
	ErrCorrupt   = errors.New("corrupt data")
	ErrTooOld    = errors.New("too old")
	ErrTooNew    = errors.New("too new")
	ErrSuspended = errors.New("suspended")
)

// LimitError is a limit a call reached, named: the bound, what the call would
// have taken of it, what it held included, and the bound's size, so that a
// caller can say which limit to raise or how much less to ask for:
//
//	resource limit: decoded samples: 120000 past 100000
//
// errors.Is finds ErrLimit through it, and Kind, an engine's own limit, when
// it has one.
type LimitError struct {
	Name   string
	Wanted int64
	Bound  int64
	Kind   error // ErrLimit when nil
}

func (e *LimitError) Error() string {
	return fmt.Sprintf("%v: %s: %d past %d", e.Unwrap(), e.Name, e.Wanted, e.Bound)
}

func (e *LimitError) Unwrap() error {
	if e.Kind == nil {
		return ErrLimit
	}
	return e.Kind
}
