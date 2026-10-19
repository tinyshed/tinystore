# Search API for Go

Every public type, function and constant of the Search engine in `tinyshed.org/tinystore/search`, generated from its source. The [Search guide](../../sql/search.md) explains how to use them, and the [Bun and Node](../bun/search.md), [Python](../python/search.md) and [Rust](../rust/search.md) pages list the same API.

## Drop

```go
func Drop(ctx context.Context, home tinystore.Home, name string) (bool, error)
```

Drop drops an index and every document it holds, a table's index its triggers off the table, from the store or the database home is; it says whether there was one.

## Reindex

```go
func Reindex(ctx context.Context, store *tinystore.Store, name string, opts ...Option) error
```

Reindex reads every document of the index again beside it, by these settings, the old answering until the new switch in, and returns once they did: what Rebuild(Manual) waits for.

## Vector

```go
func Vector(v []float32) vector
```

Vector is a document's vector on a set, or a search's on a search, of the index's dimensions, as the program's own model gave it.

## Doc

```go
type Doc[T any] struct {
	ID       any
	Value    T
	Vector   []float32
	NoVector bool
}
```

Doc is a document SetMany sets: its id, its value and what it says of its vector.

## Fields

```go
type Fields map[string]float64
```

Fields are the fields whose words are read, by their paths, each with what a word found there weighs; without them a document is its own text.

## Group

```go
type Group struct {
	// contains filtered or unexported fields
}
```

Group is conditions joined: Or's, And's.

### And

```go
func And(parts ...Where) Group
```

And is conditions every one of which passes.

### Or

```go
func Or(parts ...Where) Group
```

Or is conditions any of which passes.

## Hit

```go
type Hit[T any] struct {
	ID    string
	Value T
	Score float64
}
```

Hit is a document a search found, and the score that ordered it among this search's hits.

## Hybrid

```go
type Hybrid struct {
	VectorWeight float64
	Rerank       bool
}
```

Hybrid is how a search by words and a vector at once combines them: each half's best hundred scaled and summed at VectorWeight, 0.5 unless given, or the words' best Reranked by their vectors.

## Index

```go
type Index[T any] struct {
	// contains filtered or unexported fields
}
```

Index is documents of one type by id, or a table's rows, found again by their words, vectors or both.

### OpenIndex

```go
func OpenIndex[T any](home tinystore.Home, name string, opts ...Option) (*Index[T], error)
```

OpenIndex opens an index on the store, or a table's index on a database with Table. It opens at its first call; its error is a name or an option it refuses.

### Index.Delete

```go
func (x *Index[T]) Delete(ctx context.Context, id any) (bool, error)
```

Delete deletes the document id, and says whether there was one.

### Index.Get

```go
func (x *Index[T]) Get(ctx context.Context, id any) (T, bool, error)
```

Get is the document id, and whether there is one.

### Index.Name

```go
func (x *Index[T]) Name() string
```

Name is the index's name.

### Index.Ready

```go
func (x *Index[T]) Ready(ctx context.Context) error
```

Ready waits until the index serves the settings it was opened with; a rebuild that failed is Invalid, saying why.

### Index.Search

```go
func (x *Index[T]) Search(ctx context.Context, text string, opts ...QueryOption) (Result[T], error)
```

Search is the best ten, or Limit, by text's words, by a Vector, or by both, kept to what a Where passes. Both at once scale each half's best hundred and sum them at the index's vector weight. Of equal scores the one set first comes first.

### Index.SearchVector

```go
func (x *Index[T]) SearchVector(ctx context.Context, v []float32, opts ...QueryOption) (Result[T], error)
```

SearchVector is the best ten, or Limit, by a vector alone.

### Index.Set

```go
func (x *Index[T]) Set(ctx context.Context, id any, value T, opts ...SetOption) error
```

Set sets the document id to value, replacing it whole; the next search sees it.

### Index.SetMany

```go
func (x *Index[T]) SetMany(ctx context.Context, docs []Doc[T]) error
```

SetMany sets every document in one commit, all of them or none, 1,000 at most.

### Index.Status

```go
func (x *Index[T]) Status(ctx context.Context) (Status, error)
```

Status is where the index stands; a search never waits, and answers by the settings it serves.

## Listed

```go
type Listed struct {
	Name               string
	Documents, Vectors int
	// Opened is when a program last opened it; zero when none did.
	Opened time.Time
	Status Status
}
```

Listed is an index as List gives it.

### List

```go
func List(ctx context.Context, store *tinystore.Store) ([]Listed, error)
```

List is every index the store holds, by name.

## Option

```go
type Option interface {
	// contains filtered or unexported methods
}
```

Option is how an index opens.

### FieldNames

```go
func FieldNames(paths ...string) Option
```

FieldNames are fields whose words each weigh 1, in this order.

### Filters

```go
func Filters(paths ...string) Option
```

Filters are the fields a search's Where may keep its hits to.

### Key

```go
func Key(column string) Option
```

Key is a table's column of unique values the index names rows by; its primary key unless given.

### Rebuild

```go
func Rebuild(mode RebuildMode) Option
```

Rebuild says when a change of settings that needs the documents read again is read.

### Table

```go
func Table(name string) Option
```

Table makes the index a database's table's, opened from the database: its rows found again, each hit's value its row, the index following every change of a row in the commit that makes it.

### TypoTolerance

```go
func TypoTolerance(on bool) Option
```

TypoTolerance reads a misspelt word as the nearest the index holds, on unless turned off.

## QueryOption

```go
type QueryOption interface {
	// contains filtered or unexported methods
}
```

QueryOption is how a search reads: Vector, Where, Or, Limit, Offset, Hybrid.

### Limit

```go
func Limit(n int) QueryOption
```

Limit is how many hits a search gives, 10 unless given; Offset and Limit together 1000 at most.

### Offset

```go
func Offset(n int) QueryOption
```

Offset skips the best hits a page before gave.

## Range

```go
type Range struct {
	Gt, Gte, Lt, Lte any
}
```

Range is a field's number within bounds: nil for a bound not given.

## RebuildMode

```go
type RebuildMode bool
```

RebuildMode is when a change of settings that needs the documents read again is read.

### Automatic

```go
const (
	Automatic RebuildMode = false
	Manual    RebuildMode = true
)
```

Manual waits for Reindex to read the documents again; Automatic, the default, reads them beside the index at once.

## Result

```go
type Result[T any] struct {
	Hits      []Hit[T]
	Corrected string
}
```

Result is what a search found, the best first, and the text it read in fact when a word was misspelt: "" when none was.

## SetOption

```go
type SetOption interface {
	// contains filtered or unexported methods
}
```

SetOption is what a set says of its document's vector.

### NoVector

```go
func NoVector() SetOption
```

NoVector says the document has none, on an index with vectors: only its words find it.

## State

```go
type State string
```

State is where an index stands.

### Ready

```go
const (
	Ready      State = "ready"
	Rebuilding State = "rebuilding"
	Failed     State = "failed"
)
```

## Status

```go
type Status struct {
	State State
	Done  float64
	Error string
}
```

Status is where an index stands: ready, rebuilding with how much is Done, or a rebuild that failed and why.

## Vectors

```go
type Vectors struct {
	Dimensions int
	Recall     float64
	Column     string
}
```

Vectors is a vector the program's model gives each document, of these Dimensions, and Recall for searches from clusters, exact without it. A table's index names the Column of blobs that holds them.

## Where

```go
type Where map[string]any
```

Where keeps a search's hits to the documents whose filters pass, as sqldb's where reads a map: each field equal to its value, one of a slice, within a Range, or nil for none, every field named passing. Or and And group them.

	search.Where{"category": []string{"shoes", "boots"}, "price": search.Range{Gte: 10, Lt: 100}}

<!-- Generated by task reference from sdk/go/search/. Edit the doc comments there, not this file. -->
