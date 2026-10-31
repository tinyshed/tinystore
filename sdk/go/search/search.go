// Package search is documents found again by their words, by vectors the
// program's own model gives them, or by both, as docs/guides/search/README.md has it:
// documents of one type set by id in a named index, or a database's table
// whose rows the index follows. A search answers the exact best ten, or
// Limit, of what the index holds, kept to the documents a Where passes.
//
//	articles, err := search.OpenIndex[Article](store, "articles", search.Fields{"title": 2, "body": 1})
//	err = articles.Set(ctx, "a-17", Article{Title: "Хлеб на закваске", Body: "…"})
//	result, err := articles.Search(ctx, "как испечь хлеб")
package search

import (
	"context"
	"encoding/binary"
	"encoding/json"
	"math"
	"sort"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
	"tinyshed.org/tinystore/sqldb"
)

// readyEvery is how often Ready asks whether a rebuild switched in.
const readyEvery = 100 * time.Millisecond

// Option is how an index opens.
type Option interface{ open(*wire.SearchOpen) }

type optionFunc func(*wire.SearchOpen)

func (f optionFunc) open(o *wire.SearchOpen) { f(o) }

// Fields are the fields whose words are read, by their paths, each with what
// a word found there weighs; without them a document is its own text.
type Fields map[string]float64

func (f Fields) open(o *wire.SearchOpen) {
	paths := make([]string, 0, len(f))
	for path := range f {
		paths = append(paths, path)
	}
	sort.Strings(paths)
	for _, path := range paths {
		o.Fields = append(o.Fields, wire.SearchField{Path: path, Weight: f[path]})
	}
}

// FieldNames are fields whose words each weigh 1, in this order.
func FieldNames(paths ...string) Option {
	return optionFunc(func(o *wire.SearchOpen) {
		for _, path := range paths {
			o.Fields = append(o.Fields, wire.SearchField{Path: path, Weight: 1})
		}
	})
}

// Filters are the fields a search's Where may keep its hits to.
func Filters(paths ...string) Option {
	return optionFunc(func(o *wire.SearchOpen) { o.Filters = append(o.Filters, paths...) })
}

// TypoTolerance reads a misspelt word as the nearest the index holds, on unless turned off.
func TypoTolerance(on bool) Option {
	return optionFunc(func(o *wire.SearchOpen) { o.TypoTolerance = &on })
}

// Layouts are the keyboard layouts the index's readers type with, "ru" and
// "en" for ЙЦУКЕН and QWERTY: a word that finds nothing as typed is read as
// the same keys type in the other layout, when it finds something there,
// and Result.Corrected says so.
func Layouts(names ...string) Option {
	return optionFunc(func(o *wire.SearchOpen) { o.Layouts = append(o.Layouts, names...) })
}

// Vectors is a vector the program's model gives each document, of these
// Dimensions, and Recall for searches from clusters, exact without it. A
// table's index names the Column of blobs that holds them.
type Vectors struct {
	Dimensions int
	Recall     float64
	Column     string
}

func (v Vectors) open(o *wire.SearchOpen) {
	dimensions := uint64(max(v.Dimensions, 0))
	o.Dimensions = &dimensions
	if v.Recall != 0 {
		o.Recall = &v.Recall
	}
	if v.Column != "" {
		o.VectorColumn = &v.Column
	}
}

// Hybrid is how a search by words and a vector at once combines them: each
// half's best hundred scaled and summed at VectorWeight, 0.5 unless given, or
// the words' best Reranked by their vectors.
type Hybrid struct {
	VectorWeight float64
	Rerank       bool
}

func (h Hybrid) open(o *wire.SearchOpen) {
	if h.VectorWeight != 0 {
		o.VectorWeight = &h.VectorWeight
	}
	o.Rerank = h.Rerank
}

func (h Hybrid) search(q *wire.SearchQuery) {
	if h.VectorWeight != 0 {
		q.VectorWeight = &h.VectorWeight
	}
	q.Rerank = h.Rerank
}

// RebuildMode is when a change of settings that needs the documents read again is read.
type RebuildMode bool

// Manual waits for Reindex to read the documents again; Automatic, the
// default, reads them beside the index at once.
const (
	Automatic RebuildMode = false
	Manual    RebuildMode = true
)

// Rebuild says when a change of settings that needs the documents read again is read.
func Rebuild(mode RebuildMode) Option {
	return optionFunc(func(o *wire.SearchOpen) { o.Manual = bool(mode) })
}

// Table makes the index a database's table's, opened from the database: its
// rows found again, each hit's value its row, the index following every
// change of a row in the commit that makes it.
func Table(name string) Option { return optionFunc(func(o *wire.SearchOpen) { o.Table = &name }) }

// Key is a table's column of unique values the index names rows by; its primary key unless given.
func Key(column string) Option { return optionFunc(func(o *wire.SearchOpen) { o.Key = &column }) }

// Index is documents of one type by id, or a table's rows, found again by
// their words, vectors or both.
type Index[T any] struct {
	name   string
	opener *core.Opener
	via    core.Via
}

// OpenIndex opens an index on the store, or a table's index on a database
// with Table. It opens at its first call; its error is a name or an option it
// refuses.
func OpenIndex[T any](home tinystore.Home, name string, opts ...Option) (*Index[T], error) {
	message, err := openMessage(name, opts)
	if err != nil {
		return nil, err
	}
	of := home.CoreHome()
	index := &Index[T]{name: name}
	switch {
	case of.Database == nil && message.Table != nil:
		return nil, core.Errorf(core.Invalid, "index %s: a table's index opens from its database", name)
	case of.Database == nil:
		index.opener = &core.Opener{Method: wire.MethodSearchOpen, Body: wire.Encode(&message)}
		index.via = core.ViaLink{Link: of.Store.Link}
	case message.Table == nil:
		return nil, core.Errorf(core.Invalid, "index %s: an index opened from a database follows a table: give it Table", name)
	default:
		db := of.Database
		index.opener = db.Keep(wire.MethodSearchOpen, wire.Encode(&message), func(ctx context.Context, c *core.Connection) ([]byte, error) {
			handle, err := db.Handle(ctx, c)
			if err != nil {
				return nil, err
			}
			kept := message
			kept.Database = &handle
			return wire.Encode(&kept), nil
		})
		index.via = db.Via("index " + name)
	}
	return index, nil
}

func openMessage(name string, opts []Option) (wire.SearchOpen, error) {
	message := wire.SearchOpen{Name: name}
	if err := core.CheckName(name, "index"); err != nil {
		return message, err
	}
	for _, opt := range opts {
		opt.open(&message)
	}
	return message, nil
}

// Name is the index's name.
func (x *Index[T]) Name() string { return x.name }

func (x *Index[T]) call(ctx context.Context, method uint16, body func(handle uint64) []byte, idempotence core.Idempotence) ([]byte, error) {
	return x.via.Call(ctx, x.opener, method, body, idempotence)
}

// SetOption is what a set says of its document's vector.
type SetOption interface{ set(*vectorSaid) }

type vectorSaid struct {
	vector   *[]byte
	noVector bool
	err      error
}

// QueryOption is how a search reads: Vector, Where, Or, Prefix, PrefixEvery, Matches, Limit, Offset, Hybrid.
type QueryOption interface{ search(*wire.SearchQuery) }

type vector []float32

// Vector is a document's vector on a set, or a search's on a search, of the
// index's dimensions, as the program's own model gave it.
func Vector(v []float32) vector { return vector(v) }

func (v vector) set(s *vectorSaid) { s.vector = vectorBytes(v) }

func (v vector) search(q *wire.SearchQuery) { q.Vector = vectorBytes(v) }

func vectorBytes(v []float32) *[]byte {
	out := make([]byte, 4*len(v))
	for i, f := range v {
		binary.LittleEndian.PutUint32(out[4*i:], math.Float32bits(f))
	}
	return &out
}

type noVector struct{}

// NoVector says the document has none, on an index with vectors: only its words find it.
func NoVector() SetOption { return noVector{} }

func (noVector) set(s *vectorSaid) { s.noVector = true }

// Set sets the document id to value, replacing it whole; the next search sees it.
func (x *Index[T]) Set(ctx context.Context, id any, value T, opts ...SetOption) error {
	key, document, said, err := x.document(id, value, opts)
	if err != nil {
		return err
	}
	_, err = x.call(ctx, wire.MethodSearchSet, func(handle uint64) []byte {
		return wire.Encode(&wire.SearchSet{Handle: handle, Id: key, Value: document, Vector: said.vector, NoVector: said.noVector})
	}, core.Writing)
	return err
}

func (x *Index[T]) document(id any, value T, opts []SetOption) (string, string, vectorSaid, error) {
	var said vectorSaid
	for _, opt := range opts {
		opt.set(&said)
	}
	key, err := core.KeyText(id)
	if err != nil {
		return "", "", said, err
	}
	document, err := json.Marshal(value)
	if err != nil {
		return "", "", said, core.Wrap(core.Invalid, err, "index %s: a document that does not write as JSON", x.name)
	}
	return key, string(document), said, nil
}

// Doc is a document SetMany sets: its id, its value and what it says of its vector.
type Doc[T any] struct {
	ID       any
	Value    T
	Vector   []float32
	NoVector bool
}

// SetMany sets every document in one commit, all of them or none. A call
// holds 1,000 documents at most, limits.SetManyDocuments, and no more than one
// request body holds, 2 MiB, limits.RequestBytes. Past either it fails with a
// limit error that names the bound.
func (x *Index[T]) SetMany(ctx context.Context, docs []Doc[T]) error {
	documents := make([]wire.SearchDocument, len(docs))
	for i, doc := range docs {
		var opts []SetOption
		switch {
		case doc.Vector != nil:
			opts = append(opts, Vector(doc.Vector))
		case doc.NoVector:
			opts = append(opts, NoVector())
		}
		key, document, said, err := x.document(doc.ID, doc.Value, opts)
		if err != nil {
			return err
		}
		documents[i] = wire.SearchDocument{Id: key, Value: document, Vector: said.vector, NoVector: said.noVector}
	}
	_, err := x.call(ctx, wire.MethodSearchSetMany, func(handle uint64) []byte {
		return wire.Encode(&wire.SearchSetMany{Handle: handle, Documents: documents})
	}, core.Writing)
	return err
}

// Delete deletes the document id, and says whether there was one.
func (x *Index[T]) Delete(ctx context.Context, id any) (bool, error) {
	key, err := core.KeyText(id)
	if err != nil {
		return false, err
	}
	answer, err := x.call(ctx, wire.MethodSearchDelete, func(handle uint64) []byte {
		return wire.Encode(&wire.SearchKey{Handle: handle, Id: key})
	}, core.Writing)
	if err != nil {
		return false, err
	}
	var deleted wire.SearchDeleted
	err = decode(answer, &deleted)
	return deleted.Deleted, err
}

// Get is the document id, and whether there is one.
func (x *Index[T]) Get(ctx context.Context, id any) (T, bool, error) {
	var out T
	key, err := core.KeyText(id)
	if err != nil {
		return out, false, err
	}
	answer, err := x.call(ctx, wire.MethodSearchGet, func(handle uint64) []byte {
		return wire.Encode(&wire.SearchKey{Handle: handle, Id: key})
	}, core.Reading)
	if err != nil {
		return out, false, err
	}
	var got wire.SearchGot
	if err := decode(answer, &got); err != nil || !got.Found || got.Value == nil {
		return out, false, err
	}
	if err := json.Unmarshal([]byte(*got.Value), &out); err != nil {
		return out, false, core.Wrap(core.Invalid, err, "index %s: a document that does not read as %T", x.name, out)
	}
	return out, true, nil
}

// Hit is a document a search found, and the score that ordered it among
// this search's hits; Matches says where the search's words matched it,
// when the search asked with Matches.
type Hit[T any] struct {
	ID      string
	Value   T
	Score   float64
	Matches []Match
}

// Match is where a search's word matched a hit: bytes Start to End of a
// field's text, so that text[m.Start:m.End] is what matched. Field is its
// path, or a table's column, and "" for a document that is its own text;
// Item is the string's place in a field that holds a list, 0 in one that
// holds a string; Word is the query's word, as its text wrote it.
type Match struct {
	Field string
	Item  int
	Start int
	End   int
	Word  string
}

// Result is what a search found, the best first, and the text it read in
// fact when a word was misspelt: "" when none was.
type Result[T any] struct {
	Hits      []Hit[T]
	Corrected string
}

type prefix struct{}

// Prefix reads the last word as the start of a word too, as a search box
// types: "crypt" finds "cryptography" as well, at half its field's weight.
// A word a space follows is whole, and so is one of a single letter.
func Prefix() QueryOption { return prefix{} }

func (prefix) search(q *wire.SearchQuery) { q.Prefix = true }

type prefixEvery struct{}

// PrefixEvery reads every word as the start of a word too, as people
// shorten each word of a name: "лин алг" finds "Линейная алгебра". Each
// word of two letters or more is read so, the first eight, whatever follows
// it; a document that holds a word for each word of the text comes first,
// its score raised above the rest. In place of Prefix.
func PrefixEvery() QueryOption { return prefixEvery{} }

func (prefixEvery) search(q *wire.SearchQuery) { q.PrefixEvery = true }

type matches struct{}

// Matches has each hit say where the search's words matched it, in
// Hit.Matches, as a search box shows them in bold: a word found as written,
// by its stem or once corrected whole, and a word a prefix began by the
// letters typed.
func Matches() QueryOption { return matches{} }

// Go slices a string by its bytes.
func (matches) search(q *wire.SearchQuery) { unit := "utf8"; q.Matches = &unit }

type limit int

// Limit is how many hits a search gives, 10 unless given; Offset and Limit together 1000 at most.
func Limit(n int) QueryOption { return limit(n) }

func (n limit) search(q *wire.SearchQuery) { l := uint64(max(int(n), 0)); q.Limit = &l }

type offset int

// Offset skips the best hits a page before gave.
func Offset(n int) QueryOption { return offset(n) }

func (n offset) search(q *wire.SearchQuery) { q.Offset = uint64(max(int(n), 0)) }

// Search is the best ten, or Limit, by text's words, by a Vector, or by
// both, kept to what a Where passes. Both at once scale each half's best
// hundred and sum them at the index's vector weight. Of equal scores the one
// set first comes first.
func (x *Index[T]) Search(ctx context.Context, text string, opts ...QueryOption) (Result[T], error) {
	query := wire.SearchQuery{Text: text}
	for _, opt := range opts {
		opt.search(&query)
	}
	if query.Where != nil && *query.Where == "" {
		return Result[T]{}, core.Errorf(core.Invalid, "index %s: a where that does not write", x.name)
	}
	answer, err := x.call(ctx, wire.MethodSearchSearch, func(handle uint64) []byte {
		query.Handle = handle
		return wire.Encode(&query)
	}, core.Reading)
	if err != nil {
		return Result[T]{}, err
	}
	var found wire.SearchFound
	if err := decode(answer, &found); err != nil {
		return Result[T]{}, err
	}
	return x.result(found)
}

// SearchVector is the best ten, or Limit, by a vector alone.
func (x *Index[T]) SearchVector(ctx context.Context, v []float32, opts ...QueryOption) (Result[T], error) {
	return x.Search(ctx, "", append([]QueryOption{Vector(v)}, opts...)...)
}

func (x *Index[T]) result(found wire.SearchFound) (Result[T], error) {
	out := Result[T]{Hits: make([]Hit[T], len(found.Hits))}
	if found.Corrected != nil {
		out.Corrected = *found.Corrected
	}
	var columns []string
	if found.Columns != nil {
		columns = *found.Columns
	}
	for i, hit := range found.Hits {
		out.Hits[i] = Hit[T]{ID: hit.Id, Score: hit.Score, Matches: matchesOf(hit.Matches)}
		var err error
		switch {
		case hit.Row != nil:
			out.Hits[i].Value, err = sqldb.ReadRow[T](columns, *hit.Row)
		case hit.Value != "":
			err = json.Unmarshal([]byte(hit.Value), &out.Hits[i].Value)
		}
		if err != nil {
			return out, core.Wrap(core.Invalid, err, "index %s: a hit that does not read as %T", x.name, out.Hits[i].Value)
		}
	}
	return out, nil
}

// matchesOf is where a hit matched, an empty list when the search asked and
// none, and nil when it did not ask.
func matchesOf(found *[]wire.SearchMatch) []Match {
	if found == nil {
		return nil
	}
	out := make([]Match, len(*found))
	for i, m := range *found {
		out[i] = Match{Field: m.Field, Start: int(m.Start), End: int(m.End), Word: m.Word}
		if m.Item != nil {
			out[i].Item = int(*m.Item)
		}
	}
	return out
}

// State is where an index stands.
type State string

const (
	Ready      State = "ready"
	Rebuilding State = "rebuilding"
	Failed     State = "failed"
)

// Status is where an index stands: ready, rebuilding with how much is Done,
// or a rebuild that failed and why.
type Status struct {
	State State
	Done  float64
	Error string
}

// Status is where the index stands; a search never waits, and answers by the settings it serves.
func (x *Index[T]) Status(ctx context.Context) (Status, error) {
	answer, err := x.call(ctx, wire.MethodSearchStatus, func(handle uint64) []byte {
		return wire.Encode(&wire.Handle{Handle: handle})
	}, core.Reading)
	if err != nil {
		return Status{}, err
	}
	var status wire.SearchStatus
	err = decode(answer, &status)
	return statusOf(status.State, status.Done, status.Error), err
}

func statusOf(state string, done float64, why string) Status {
	switch state {
	case "rebuilding":
		return Status{State: Rebuilding, Done: done}
	case "failed":
		return Status{State: Failed, Error: why}
	}
	return Status{State: Ready}
}

// Ready waits until the index serves the settings it was opened with; a
// rebuild that failed is Invalid, saying why.
func (x *Index[T]) Ready(ctx context.Context) error {
	for {
		status, err := x.Status(ctx)
		switch {
		case err != nil:
			return err
		case status.State == Ready:
			return nil
		case status.State == Failed:
			return core.Errorf(core.Invalid, "index %s: its rebuild failed: %s", x.name, status.Error)
		}
		select {
		case <-time.After(readyEvery):
		case <-ctx.Done():
			return ctx.Err()
		}
	}
}

// Reindex reads every document of the index again beside it, by these
// settings, the old answering until the new switch in, and returns once they
// did: what Rebuild(Manual) waits for.
func Reindex(ctx context.Context, store *tinystore.Store, name string, opts ...Option) error {
	message, err := openMessage(name, opts)
	if err != nil {
		return err
	}
	message.Manual, message.Reindex = true, true
	link := store.CoreHome().Store.Link
	reindexed := &Index[any]{name: name, opener: &core.Opener{Method: wire.MethodSearchOpen, Body: wire.Encode(&message)}, via: core.ViaLink{Link: link}}
	return reindexed.Ready(ctx)
}

// Drop drops an index and every document it holds, a table's index its
// triggers off the table, from the store or the database home is; it says
// whether there was one.
func Drop(ctx context.Context, home tinystore.Home, name string) (bool, error) {
	if err := core.CheckName(name, "index"); err != nil {
		return false, err
	}
	of := home.CoreHome()
	var dropped wire.SearchDropped
	err := of.Store.Link.Run(ctx, core.Writing, func(c *core.Connection) error {
		message := wire.SearchName{Name: name}
		if of.Database != nil {
			handle, err := of.Database.Handle(ctx, c)
			if err != nil {
				return err
			}
			message.Database = &handle
		}
		answer, err := c.Session.Call(ctx, wire.MethodSearchDropIndex, wire.Encode(&message))
		if err != nil {
			return err
		}
		return decode(answer, &dropped)
	})
	return dropped.Dropped, err
}

// Listed is an index as List gives it.
type Listed struct {
	Name               string
	Documents, Vectors int
	// Opened is when a program last opened it; zero when none did.
	Opened time.Time
	Status Status
}

// List is every index the store holds, by name.
func List(ctx context.Context, store *tinystore.Store) ([]Listed, error) {
	answer, err := store.CoreHome().Store.Link.Call(ctx, core.Reading, wire.MethodSearchList, wire.Encode(&wire.Empty{}))
	if err != nil {
		return nil, err
	}
	var listed wire.SearchListed
	if err := decode(answer, &listed); err != nil {
		return nil, err
	}
	out := make([]Listed, len(listed.Indexes))
	for i, each := range listed.Indexes {
		out[i] = Listed{Name: each.Name, Documents: int(each.Documents), Vectors: int(each.Vectors), Status: statusOf(each.State, each.Done, each.Error)}
		if each.Opened != nil {
			out[i].Opened = time.UnixMilli(*each.Opened).UTC()
		}
	}
	return out, nil
}

func decode(answer []byte, m wire.Message) error {
	if err := wire.Decode(answer, m); err != nil {
		return core.Wrap(core.Protocol, err, "an answer that does not read")
	}
	return nil
}
