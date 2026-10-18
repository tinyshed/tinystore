package core

import (
	"errors"
	"fmt"
	"strconv"
	"strings"
)

// ErrorKind is what failed, the same in every engine, on the wire and in
// every SDK: the code the store's error names.
type ErrorKind string

// The kinds an error has, by the codes the store's errors name.
const (
	Invalid          ErrorKind = "invalid"
	NotFound         ErrorKind = "not_found"
	Limit            ErrorKind = "limit"
	Closed           ErrorKind = "closed"
	InUse            ErrorKind = "in_use"
	Conflict         ErrorKind = "conflict"
	Corrupt          ErrorKind = "corrupt"
	TooOld           ErrorKind = "too_old"
	TooNew           ErrorKind = "too_new"
	Suspended        ErrorKind = "suspended"
	OutcomeUnknown   ErrorKind = "outcome_unknown"
	CommittedLocally ErrorKind = "committed_locally"
	Lost             ErrorKind = "lost"
	PermissionDenied ErrorKind = "permission"
	Unimplemented    ErrorKind = "unimplemented"
	Cancelled        ErrorKind = "cancelled"
	Unavailable      ErrorKind = "unavailable"
	Internal         ErrorKind = "internal"
	Protocol         ErrorKind = "protocol"
	Unauthenticated  ErrorKind = "unauthenticated"
)

// Kinds are every kind this SDK knows; one it does not know yet is Internal,
// keeping its code in the message.
var Kinds = []ErrorKind{
	Invalid, NotFound, Limit, Closed, InUse, Conflict, Corrupt, TooOld, TooNew, Suspended, OutcomeUnknown,
	CommittedLocally, Lost, PermissionDenied, Unimplemented, Cancelled, Unavailable, Internal, Protocol,
	Unauthenticated,
}

// Error is a failure of the store or of the SDK: what failed, the item it is
// about, and the cause it wraps.
//
//	var e *tinystore.Error
//	if errors.As(err, &e) && e.Kind == tinystore.Conflict { … }
type Error struct {
	Kind    ErrorKind
	Message string
	// What names the item a failure is about, by the names the store gives:
	// a bucket and a key, a queue and an id, a table and a constraint.
	What map[string]string
	// Constraint is the constraint a database's write broke; nil for any other failure.
	Constraint *Constraint
	// Limit, Wanted and Bound say which bound a limit error met, what the call
	// would have taken of it, and the bound: "decoded samples", 120000, 100000.
	Limit  string
	Wanted int64
	Bound  int64
	cause  error
	// ended says the connection ended under a stream, and sent whether the
	// stream's REQUEST had left
	ended, sent bool
}

func (e *Error) Error() string {
	if e.cause != nil && !strings.Contains(e.Message, e.cause.Error()) {
		return e.Message + ": " + e.cause.Error()
	}
	return e.Message
}

func (e *Error) Unwrap() error { return e.cause }

// Is matches a sentinel of the same kind, so that errors.Is(err, ErrConflict) holds.
func (e *Error) Is(target error) bool {
	var other *Error
	if !errors.As(target, &other) {
		return false
	}
	return other.Message == "" && other.Kind == e.Kind
}

// Constraint is a constraint a database's write broke, as SQLite names it. A
// unique or a primary key already held is a Conflict, any other Invalid.
type Constraint struct {
	// Kind is unique, primaryKey, foreignKey, check or notNull.
	Kind    string
	Table   string
	Columns []string
	// Name is a check's name, or its text when it has none.
	Name string
}

// The sentinels errors.Is compares with, one a kind.
var (
	ErrInvalid          = &Error{Kind: Invalid}
	ErrNotFound         = &Error{Kind: NotFound}
	ErrLimit            = &Error{Kind: Limit}
	ErrClosed           = &Error{Kind: Closed}
	ErrInUse            = &Error{Kind: InUse}
	ErrConflict         = &Error{Kind: Conflict}
	ErrCorrupt          = &Error{Kind: Corrupt}
	ErrTooOld           = &Error{Kind: TooOld}
	ErrTooNew           = &Error{Kind: TooNew}
	ErrSuspended        = &Error{Kind: Suspended}
	ErrOutcomeUnknown   = &Error{Kind: OutcomeUnknown}
	ErrCommittedLocally = &Error{Kind: CommittedLocally}
	ErrLost             = &Error{Kind: Lost}
	ErrPermissionDenied = &Error{Kind: PermissionDenied}
	ErrUnimplemented    = &Error{Kind: Unimplemented}
	ErrCancelled        = &Error{Kind: Cancelled}
	ErrUnavailable      = &Error{Kind: Unavailable}
	ErrInternal         = &Error{Kind: Internal}
	ErrProtocol         = &Error{Kind: Protocol}
	ErrUnauthenticated  = &Error{Kind: Unauthenticated}
)

// Errorf is an error of kind, its message formatted.
func Errorf(kind ErrorKind, format string, args ...any) *Error {
	return &Error{Kind: kind, Message: fmt.Sprintf(format, args...)}
}

// Wrap is an error of kind whose message says what failed and wraps cause.
func Wrap(kind ErrorKind, cause error, format string, args ...any) *Error {
	return &Error{Kind: kind, Message: fmt.Sprintf(format, args...), cause: cause}
}

// Failed is the error a code names, as the store's failure carries it.
func Failed(code, message string, what map[string]string) *Error {
	kind := ErrorKind(code)
	known := false
	for _, k := range Kinds {
		if k == kind {
			known = true
			break
		}
	}
	if !known {
		kind, message = Internal, code+": "+message
	}
	e := &Error{Kind: kind, Message: message, What: what}
	e.Constraint = constraintOf(what)
	if kind == Limit && what != nil {
		e.Limit = what["limit"]
		e.Wanted, _ = strconv.ParseInt(what["wanted"], 10, 64)
		e.Bound, _ = strconv.ParseInt(what["bound"], 10, 64)
	}
	return e
}

func constraintOf(what map[string]string) *Constraint {
	switch what["constraint"] {
	case "unique", "primaryKey", "foreignKey", "check", "notNull":
	default:
		return nil
	}
	c := &Constraint{Kind: what["constraint"], Table: what["table"], Name: what["name"]}
	if columns := what["columns"]; columns != "" {
		c.Columns = strings.Split(columns, ", ")
	}
	return c
}

// KindOf is the kind of an error of this SDK, or "" for any other.
func KindOf(err error) ErrorKind {
	var e *Error
	if errors.As(err, &e) {
		return e.Kind
	}
	return ""
}

// lost is a connection that ended with a stream on it, saying whether its
// REQUEST had left: one that had not may go on another connection whatever
// it was, and one that had is a read to send again or a write whose outcome
// is unknown.
func lost(cause error, sent bool) *Error {
	return &Error{Kind: Closed, Message: "the connection ended", cause: cause, ended: true, sent: sent}
}

// IsConnectionLost says whether err is a connection that ended under a call,
// and whether the call's REQUEST had left.
func IsConnectionLost(err error) (lost bool, sent bool) {
	var e *Error
	if errors.As(err, &e) && e.ended {
		return true, e.sent
	}
	return false, false
}
