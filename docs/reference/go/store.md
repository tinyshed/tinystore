# Store API for Go

Every public type, function and constant of the store in `tinyshed.org/tinystore`, generated from its source. The [Store guide](../../languages.md) explains how to use them, and the [Bun and Node](../bun/store.md), [Python](../python/store.md) and [Rust](../rust/store.md) pages list the same API.

## Invalid

```go
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
```

The kinds an error has.

## ErrInvalid

```go
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
```

The sentinels errors.Is compares an error's kind with: errors.Is(err, tinystore.ErrConflict).

## ConnectOption

```go
type ConnectOption func(*connectOptions)
```

ConnectOption is how a remote server is reached.

### TLS

```go
func TLS(config *tls.Config) ConnectOption
```

TLS is the configuration a tls:// server's certificate is checked with: the system's roots unless given.

## Constraint

```go
type Constraint = core.Constraint
```

Constraint is a constraint a database's write broke, as SQLite names it.

## Error

```go
type Error = core.Error
```

Error is a failure of the store or of the SDK: its kind, its message, the item it is about, and the cause it wraps.

	var e *tinystore.Error
	if errors.As(err, &e) && e.Kind == tinystore.Conflict && e.Constraint != nil { … }

## ErrorKind

```go
type ErrorKind = core.ErrorKind
```

ErrorKind is what failed, the same in every engine, on the wire and in every SDK.

## Home

```go
type Home = core.Home
```

Home is where a handle opens: the store, or a database of it whose file keeps the handle. \*Store and \*sqldb.DB are homes.

## InTx

```go
type InTx = core.InTx
```

InTx is a transaction a handle is taken into with its In: the store's, \*tinystore.Tx, or a database's, \*sqldb.Tx.

## Option

```go
type Option func(*options)
```

Option is how a store opens.

### Binary

```go
func Binary(path string) Option
```

Binary is the tinystore binary a sidecar or a private child runs; TINYSTORE\_BIN or PATH's unless given.

### Clock

```go
func Clock(start time.Time) Option
```

Clock runs a private server on a test's clock, starting at this time, which store.Clock() moves forward instead of a test waiting.

### Durability

```go
func Durability(level string) Option
```

Durability is how far a commit of the store's files goes before it returns: "full", the default, is synced to the disk; "os" survives the program's crash, and a power loss may take the last commits. It is the store's, set by whoever opens it first: one found open with another is refused.

### Embedded

```go
func Embedded() Option
```

Embedded runs the core inside this process, through the library Library names or TINYSTORE\_LIBRARY. On Linux and macOS it needs cgo.

### EncryptionKeyFile

```go
func EncryptionKeyFile(path string) Option
```

EncryptionKeyFile is the file of the store's encryption key, 64 hex digits, kept apart from the data; the store reads it and never makes it.

### Idle

```go
func Idle(d time.Duration) Option
```

Idle is how long a sidecar this process starts stays once its last connection has gone: 30 s unless given, 0 for ever. One found running keeps its own.

### Library

```go
func Library(path string) Option
```

Library is the core's library the store in this process loads: tinystore\_ffi.dll, libtinystore\_ffi.so or .dylib.

### Private

```go
func Private() Option
```

Private starts a server of this process's own, a child that lives and dies with it, for tests and scripts.

### SelfMetrics

```go
func SelfMetrics(opts ...SelfMetricsOption) Option
```

SelfMetrics measures the store into its own metrics, under names that start with tinystore\_: its commits and their waits, its files, its memory and the machine it runs on, which WithoutSystem leaves out. Off unless asked for; a sidecar found running keeps its own.

## SelfMetricsOption

```go
type SelfMetricsOption func(*options)
```

SelfMetricsOption is what a store's own metrics leave out.

### WithoutSystem

```go
func WithoutSystem() SelfMetricsOption
```

WithoutSystem leaves the machine out of the store's own metrics: its process's memory and processor, and the system's.

## Status

```go
type Status struct {
	// Server is its version: v0.1.0, or (devel) for one built from a checkout.
	Server   string
	Protocol uint64
	// Engines are the engines it serves.
	Engines []string
	// Capability is admin, data or read: admin may change a schema, data
	// reads and writes, read only reads.
	Capability string
}
```

Status is what a server is, as its WELCOME said it to this store's connection.

## Store

```go
type Store struct {
	// contains filtered or unexported fields
}
```

Store is the store in a directory, as this process reaches it. Close it once: its close stops every worker, sends what instruments changed, hands over the loggers' lines, and lets the directory go.

### Connect

```go
func Connect(ctx context.Context, url, token string, opts ...ConnectOption) (*Store, error)
```

Connect connects to a remote server with a token. tls:// checks the server's certificate before the token leaves; tcp:// sends the token in the clear, for a network its operator trusts.

### Open

```go
func Open(ctx context.Context, directory string, opts ...Option) (*Store, error)
```

Open opens the store in a directory, and returns once it answers: through the directory's sidecar, found through its SERVE file or started, unless Private or Embedded says otherwise. A directory that cannot be served fails here.

### Store.Clock

```go
func (s *Store) Clock() *TestClock
```

Clock is the test's clock of a private store opened with tinystore.Clock; on any other store its calls are Invalid.

### Store.Close

```go
func (s *Store) Close() error
```

Close stops every worker, each once its handlers under way have answered, sends what instruments changed, then closes the connection. A private child or the core in this process lets the directory go before it returns; the directory's sidecar goes once it has been idle.

### Store.CoreHome

```go
func (s *Store) CoreHome() core.HomeOf
```

CoreHome is how this module's packages reach the store; a program has no use for it.

### Store.Now

```go
func (s *Store) Now() time.Time
```

Now is the time on this machine, which a row written now is stamped with.

### Store.Status

```go
func (s *Store) Status(ctx context.Context) (Status, error)
```

Status is what the server this store reaches is: its version, protocol, engines and capability.

### Store.Tx

```go
func (s *Store) Tx(ctx context.Context, fn func(tx *Tx) error) error
```

Tx runs fn as one transaction over the handles it takes in with In: reads go at once; writes commit together when fn returns, after a check that nothing it read has changed, and fn runs again when something did, five times at most, so it does nothing else that must happen once. An error fn returns rolls it back and is Tx's.

## TestClock

```go
type TestClock struct {
	// contains filtered or unexported fields
}
```

TestClock is the clock a private server opened with tinystore.Clock runs on: moving it forward expires keys, makes jobs due and ages records at once.

### TestClock.Advance

```go
func (c *TestClock) Advance(ctx context.Context, d time.Duration) error
```

Advance moves the clock forward by d.

### TestClock.Now

```go
func (c *TestClock) Now(ctx context.Context) (time.Time, error)
```

Now is the time the server's clock reads.

### TestClock.Set

```go
func (c *TestClock) Set(ctx context.Context, to time.Time) error
```

Set sets the clock to a time, which may not be before its own.

## Tx

```go
type Tx struct {
	// contains filtered or unexported fields
}
```

Tx is the store's transaction while its function runs: a handle takes it in with In, stock.In(tx), and its calls then go through it.

### Tx.CoreTx

```go
func (tx *Tx) CoreTx() core.TxOf
```

CoreTx is how this module's packages reach the transaction; a program has no use for it.

<!-- Generated by task reference from sdk/go/store.go, sdk/go/tinystore.go. Edit the doc comments there, not this file. -->
