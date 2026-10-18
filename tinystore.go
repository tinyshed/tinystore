// Package tinystore opens a store: kv, jobs, SQL databases, files, events,
// metrics, configs, locks, channels and search in one directory, reached
// through the core in this process, the directory's sidecar, a private child
// or a remote server, over protocol 2.
//
//	store, err := tinystore.Open(ctx, "data")
//	if err != nil {
//		return err
//	}
//	defer store.Close()
//	sessions, err := kv.OpenBucket[Session](store, "sessions", kv.Idle(30*24*time.Hour))
//
// Each engine is a package of its own, kv, jobs, sqldb, blobs, records,
// metrics, locks, channels and search, whose handles open on a store, or on
// a database whose file keeps them.
package tinystore

import "github.com/tinyshed/tinystore/internal/core"

// Error is a failure of the store or of the SDK: its kind, its message, the
// item it is about, and the cause it wraps.
//
//	var e *tinystore.Error
//	if errors.As(err, &e) && e.Kind == tinystore.Conflict && e.Constraint != nil { … }
type Error = core.Error

// ErrorKind is what failed, the same in every engine, on the wire and in every SDK.
type ErrorKind = core.ErrorKind

// Constraint is a constraint a database's write broke, as SQLite names it.
type Constraint = core.Constraint

// The kinds an error has.
const (
	Invalid          = core.Invalid
	NotFound         = core.NotFound
	Limit            = core.Limit
	Closed           = core.Closed
	InUse            = core.InUse
	Conflict         = core.Conflict
	Corrupt          = core.Corrupt
	TooOld           = core.TooOld
	TooNew           = core.TooNew
	Suspended        = core.Suspended
	OutcomeUnknown   = core.OutcomeUnknown
	CommittedLocally = core.CommittedLocally
	Lost             = core.Lost
	PermissionDenied = core.PermissionDenied
	Unimplemented    = core.Unimplemented
	Cancelled        = core.Cancelled
	Unavailable      = core.Unavailable
	Internal         = core.Internal
	Protocol         = core.Protocol
	Unauthenticated  = core.Unauthenticated
)

// The sentinels errors.Is compares an error's kind with:
// errors.Is(err, tinystore.ErrConflict).
var (
	ErrInvalid          = core.ErrInvalid
	ErrNotFound         = core.ErrNotFound
	ErrLimit            = core.ErrLimit
	ErrClosed           = core.ErrClosed
	ErrInUse            = core.ErrInUse
	ErrConflict         = core.ErrConflict
	ErrCorrupt          = core.ErrCorrupt
	ErrTooOld           = core.ErrTooOld
	ErrTooNew           = core.ErrTooNew
	ErrSuspended        = core.ErrSuspended
	ErrOutcomeUnknown   = core.ErrOutcomeUnknown
	ErrCommittedLocally = core.ErrCommittedLocally
	ErrLost             = core.ErrLost
	ErrPermissionDenied = core.ErrPermissionDenied
	ErrUnimplemented    = core.ErrUnimplemented
	ErrCancelled        = core.ErrCancelled
	ErrUnavailable      = core.ErrUnavailable
	ErrInternal         = core.ErrInternal
	ErrProtocol         = core.ErrProtocol
	ErrUnauthenticated  = core.ErrUnauthenticated
)

// Home is where a handle opens: the store, or a database of it whose file
// keeps the handle. *Store and *sqldb.DB are homes.
type Home = core.Home

// InTx is a transaction a handle is taken into with its In: the store's,
// *tinystore.Tx, or a database's, *sqldb.Tx.
type InTx = core.InTx
