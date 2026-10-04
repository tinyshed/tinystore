# Store API for Go

Every public type, function and constant of the store in `github.com/tinyshed/tinystore`, generated from its source. The [Store guide](../../languages.md) explains how to use them, and the [Bun and Node](../bun/store.md) and [Python](../python/store.md) pages list the same API.

## LimitMemory

```go
const (
	LimitMemory    = "store memory"      // what one call would hold at once
	LimitMemoryNow = "store memory, now" // what the calls running hold together
)
```

LimitError is a limit a call reached, named: the bound, what the call would have taken of it, what it held included, and the bound's size, so that a caller can say which limit to raise or how much less to ask for:

	resource limit: decoded samples: 120000 past 100000

errors.Is finds ErrLimit through it, and Kind, an engine's own limit, when it has one. The names of the store's own limits, which a LimitError's Name holds.

## ErrInvalid

```go
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
```

Every engine wraps these, so errors.Is means the same in all of them.

## FromCgroup

```go
func FromCgroup(fraction float64) int64
```

FromCgroup is fraction of the memory the process's container may use, for Options.Memory: cgroup v2's memory.max, or v1's memory.limit\_in\_bytes. It is 0, no budget, outside Linux and when the container sets no limit.

	tinystore.Options{Memory: tinystore.FromCgroup(0.5)}  // half of a 32 MiB container: 16 MiB

## SnapshotPath

```go
func SnapshotPath(dir, name string) string
```

SnapshotPath is where an engine writes its copy of name inside dir.

## Engine

```go
type Engine interface {
	Close(ctx context.Context) error
}
```

## LimitError

```go
type LimitError struct {
	Name   string
	Wanted int64
	Bound  int64
	Kind   error // ErrLimit when nil
}
```

### LimitError.Error

```go
func (e *LimitError) Error() string
```

### LimitError.Unwrap

```go
func (e *LimitError) Unwrap() error
```

## Measure

```go
type Measure struct {
	Engine, Name string
	Value        uint64
	Counter      bool
}
```

Measure is an integer counter or gauge reported by an engine. Names have bounded, fixed cardinality: do not put a bucket, queue, key or path in them.

## MemoryUsage

```go
type MemoryUsage struct {
	Used, Peak, Capacity int64
}
```

MemoryUsage is the store's budget: Capacity is Options.Memory, zero when every engine keeps only its own per-call limits.

## Options

```go
type Options struct {
	// Logger receives TinyStore's own logs; nil discards them.
	Logger *slog.Logger

	// Manual runs no background work: the application calls each engine's
	// maintenance itself. Tests usually want this.
	Manual bool

	// Clock replaces time.Now for the store and every engine opened against it.
	Clock func() time.Time

	// Memory bounds the bytes that all engines' in-flight work holds at once;
	// zero leaves each engine to its own per-call limits.
	Memory int64

	// Guest opens a directory another store holds, without its LOCK, as a
	// second process beside a running program opens it to reset a password:
	// nothing runs in the background, and only SQL databases open in it,
	// applying no migration and changing no schema, nor the store's own tables.
	// SQLite's locks share each file's writer between the two processes, and a
	// guest's write waits up to five seconds for the owner's.
	Guest bool

	// Readers bounds the reader connections each engine's file opens under
	// load, half a MiB each; zero leaves each engine its own count. A reader
	// beyond one closes after a minute unused, whatever the bound.
	Readers int

	// SelfMetrics periodically writes available engine reports into an opened
	// metrics engine. Manual stores call FlushSelfMetrics themselves.
	SelfMetrics bool
}
```

## Reporter

```go
type Reporter interface {
	Report() []Measure
}
```

## Reservation

```go
type Reservation struct {
	// contains filtered or unexported fields
}
```

Reservation is bytes of the store's memory that one piece of work holds. The zero Reservation holds nothing, for work that needs no memory.

### Reservation.Release

```go
func (r *Reservation) Release()
```

Release gives back what the reservation holds; a second call gives nothing.

### Reservation.Shrink

```go
func (r *Reservation) Shrink(n int64)
```

Shrink gives back all but n of the bytes, for work that reserved its worst case and has learnt what it holds.

## SelfWriter

```go
type SelfWriter interface {
	WriteSelf(context.Context, []Measure) error
}
```

SelfWriter writes one report without counting that write as user ingest. The root uses this interface without importing the metrics engine.

## Snapshot

```go
type Snapshot struct {
	Dir   string
	Files []SnapshotFile
}
```

Snapshot is a copy of every engine's file, in Dir inside the store's directory, so that it takes the space of the disk the data is on.

### Snapshot.Remove

```go
func (s Snapshot) Remove() error
```

## SnapshotFile

```go
type SnapshotFile struct {
	// Name is the file's path inside the store, such as "metrics.db",
	// "sql/app.db" or "blobs/objects/1a3/1a3f07".
	Name   string `json:"name"`
	Engine string `json:"engine"`
	// Schema is how many migrations the engine's database has.
	Schema int `json:"schema"`
	// Stored asks a backup to keep the bytes as they are rather than deflate
	// them: an application's files are mostly media, which does not compress.
	Stored bool `json:"-"`
}
```

SnapshotFile is one file of an engine's copy.

## Snapshotter

```go
type Snapshotter interface {
	Snapshot(ctx context.Context, dir string) ([]SnapshotFile, error)
}
```

Snapshotter is an engine that can copy its files while it keeps working.

## Store

```go
type Store struct {
	// contains filtered or unexported fields
}
```

### Open

```go
func Open(ctx context.Context, dir string, options Options) (*Store, error)
```

Open creates dir if needed and holds it until Close; a second store on the same directory, in this process or another, is refused with ErrInUse. ctx bounds the opening only: background work lives until Close.

### Store.Attach

```go
func (s *Store) Attach(engine Engine) error
```

Attach hands an opened engine to the store, which closes it on Close.

### Store.Claim

```go
func (s *Store) Claim(name string) (filePath string, release func(), err error)
```

Claim reserves a file or directory inside the store for one engine and returns its path, creating the directory it lies in, or the directory itself for a name that ends in /. release gives the name back to an engine that failed to open; an attached engine keeps it until the store closes.

	metrics.db  records.db  blobs/    engines
	sql/<name>.db                     databases the application names

### Store.Close

```go
func (s *Store) Close(ctx context.Context) error
```

Close stops background work, closes every engine, the last opened first, and releases the directory. When ctx ends first Close returns its error, and the cleanup still finishes; a later Close waits for it.

### Store.Dir

```go
func (s *Store) Dir() string
```

Dir is the directory the store holds. A tool that backs the store up looks there for the files of engines no one has opened.

### Store.Every

```go
func (s *Store) Every(name string, interval time.Duration, work func(context.Context) error) (soon func())
```

Every runs work at each interval until the store closes, and soon asks for its next run now rather than at the next interval: asks that come while it runs or waits to run make one run more. A Manual store runs nothing, and its soon does nothing; a failure is logged, and the next run still happens.

### Store.EveryEngine

```go
func (s *Store) EveryEngine(engine, name string, interval time.Duration, work func(context.Context) error) (
	soon func(),
)
```

EveryEngine runs engine work with the same engine attribute as Store.Logger.

### Store.FlushSelfMetrics

```go
func (s *Store) FlushSelfMetrics(ctx context.Context) error
```

FlushSelfMetrics captures available reports and the store's memory budget once, then commits their samples together. It is a no-op when disabled or when no metrics engine is open. It reports reserved bytes, not process RSS.

### Store.Guest

```go
func (s *Store) Guest() bool
```

Guest says whether the store was opened with Options.Guest.

### Store.Logger

```go
func (s *Store) Logger(engine string) *slog.Logger
```

Logger is the application's logger, labelled with the engine that logs.

### Store.Memory

```go
func (s *Store) Memory() MemoryUsage
```

### Store.Now

```go
func (s *Store) Now() time.Time
```

### Store.Readers

```go
func (s *Store) Readers(want int) int
```

Readers is how many reader connections an engine that wants want opens: want, or Options.Readers when that is fewer.

### Store.Reserve

```go
func (s *Store) Reserve(ctx context.Context, bytes int64) (*Reservation, error)
```

Reserve waits, in arrival order, until bytes fit the store's memory. A reservation larger than the whole budget is refused at once with ErrLimit, and a store without Options.Memory grants every reservation.

### Store.ReserveNow

```go
func (s *Store) ReserveNow(bytes int64) (*Reservation, error)
```

ReserveNow takes bytes only if they fit at once, ahead of any reservation waiting, and is ErrLimit otherwise. Work holding what a waiting reservation may need, such as a file's writer, reserves this way: waiting, it could wait for itself.

### Store.Snapshot

```go
func (s *Store) Snapshot(ctx context.Context) (Snapshot, error)
```

Snapshot copies every attached engine's file. Each copy is one moment of its engine; two engines are two moments, as no write spans two engines anyway. The engines keep working, and Close waits for the copying to finish.

<!-- Generated by task reference from the root package. Edit the doc comments there, not this file. -->
