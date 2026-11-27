package search

import (
	"context"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// AmongOption is how one index of a search among several is searched:
// Boost, Limit, Offset, a Where, Prefix, PrefixEvery, Matches and Snippet.
// A vector or a Total belong to a search of one index alone.
type AmongOption interface{ among(*wire.SearchAmong) }

// SearchOption is a QueryOption a search among several takes too.
type SearchOption interface {
	QueryOption
	AmongOption
}

type boost float64

// Boost is what an index's hits weigh in ManyResult.Merged, a positive number,
// 1 unless given.
func Boost(b float64) AmongOption { return boost(b) }

func (b boost) among(a *wire.SearchAmong) { weight := float64(b); a.Boost = &weight }

func (n limit) among(a *wire.SearchAmong)       { n.search(&a.Query) }
func (n offset) among(a *wire.SearchAmong)      { n.search(&a.Query) }
func (p prefix) among(a *wire.SearchAmong)      { p.search(&a.Query) }
func (p prefixEvery) among(a *wire.SearchAmong) { p.search(&a.Query) }
func (m matches) among(a *wire.SearchAmong)     { m.search(&a.Query) }
func (c snippet) among(a *wire.SearchAmong)     { c.search(&a.Query) }
func (w Where) among(a *wire.SearchAmong)       { w.search(&a.Query) }
func (g Group) among(a *wire.SearchAmong)       { g.search(&a.Query) }

// Asked is an index a search among several asks, from Among.
type Asked struct {
	name   string
	opener *core.Opener
	store  *core.Store
	among  wire.SearchAmong
	hits   func([]wire.SearchHit, *[]string) ([]Hit[any], error)
}

// Among is the index x in a search among several, searched as opts say:
// each of its hits' Value is a T, as a search of x alone reads it. Limit is
// 10 unless given, Offset and Limit together 1000 at most, and the group's
// More says whether x has more, its next page at Offset plus Limit.
func Among[T any](x *Index[T], opts ...AmongOption) Asked {
	asked := Asked{name: x.name, opener: x.opener, store: x.store}
	for _, opt := range opts {
		opt.among(&asked.among)
	}
	asked.hits = func(found []wire.SearchHit, columns *[]string) ([]Hit[any], error) {
		typed, err := x.hits(found, columns)
		out := make([]Hit[any], len(typed))
		for i, hit := range typed {
			out[i] = Hit[any]{
				ID: hit.ID, Value: hit.Value, Score: hit.Score, Matches: hit.Matches, Snippet: hit.Snippet,
				TextScore: hit.TextScore, Similarity: hit.Similarity,
			}
		}
		return out, err
	}
	return asked
}

// ManyGroup is an index's hits in a search among several, the best by its
// own scores first, and More, whether it has hits past its Limit; or Err,
// why its search failed, and no hit.
type ManyGroup struct {
	Index     string
	Hits      []Hit[any]
	More      bool
	Corrected string
	Err       error
}

// Merged is a hit of ManyResult.Merged: its group's index, the group's place
// among the groups, and the hit its group holds.
type Merged struct {
	Index string
	Group int
	Hit   *Hit[any]
}

// ManyResult is what a search among several indexes found: a group an
// index, in the order asked, and every group's hits in one order, Merged, by
// reciprocal rank fusion and never by score: the hit at rank r of its group,
// from 1, weighs boost / (60 + r); of equal weights the group asked first
// comes first, then the higher rank.
type ManyResult struct {
	Groups []ManyGroup
	Merged []Merged
}

// Many asks text of every index named, 1 to 32, in one call over one
// connection, each with its own options:
//
//	found, err := search.Many(ctx, store, "algebra",
//		search.Among(courses, search.Boost(2), search.Limit(5)),
//		search.Among(pages, search.Where{"space_id": mine}, search.Limit(5), search.Snippet("plain", 60)),
//		search.Among(people, search.Limit(5)))
//
// An index that fails is its group's Err while the others answer; the call
// fails only when every one does. The store's indexes are searched in one
// snapshot, a table's index in its database's, and groups of different
// files are not one moment. An index of another store is refused.
func Many(ctx context.Context, home tinystore.Home, text string, among ...Asked) (ManyResult, error) {
	store := home.CoreHome().Store
	for _, asked := range among {
		if asked.store != store {
			return ManyResult{}, core.Errorf(core.Invalid, "index %s: an index of another store, in a search among this one's", asked.name)
		}
		if where := asked.among.Query.Where; where != nil && *where == "" {
			return ManyResult{}, core.Errorf(core.Invalid, "index %s: a where that does not write", asked.name)
		}
	}
	var answer []byte
	err := store.Link.Run(ctx, core.Reading, func(c *core.Connection) error {
		query := wire.SearchManyQuery{Text: text, Among: make([]wire.SearchAmong, len(among))}
		for i, asked := range among {
			handle, err := core.HandleOn(ctx, c, asked.opener)
			if err != nil {
				return err
			}
			query.Among[i] = asked.among
			query.Among[i].Query.Handle = handle
		}
		var err error
		answer, err = c.Session.Call(ctx, wire.MethodSearchSearchMany, wire.Encode(&query))
		return err
	})
	if err != nil {
		return ManyResult{}, err
	}
	var found wire.SearchManyFound
	if err := decode(answer, &found); err != nil {
		return ManyResult{}, err
	}
	return manyResult(found, among)
}

func manyResult(found wire.SearchManyFound, among []Asked) (ManyResult, error) {
	if len(found.Groups) != len(among) {
		return ManyResult{}, core.Errorf(core.Protocol, "a search among %d indexes answered %d groups", len(among), len(found.Groups))
	}
	out := ManyResult{Groups: make([]ManyGroup, len(found.Groups)), Merged: make([]Merged, len(found.Merged))}
	for i, group := range found.Groups {
		out.Groups[i] = ManyGroup{Index: group.Index, More: group.More}
		if group.Failure != nil {
			what := map[string]string{}
			if group.Failure.What != nil {
				what = *group.Failure.What
			}
			out.Groups[i].Err = core.Failed(group.Failure.Code, group.Failure.Message, what)
			continue
		}
		if group.Corrected != nil {
			out.Groups[i].Corrected = *group.Corrected
		}
		hits, err := among[i].hits(group.Hits, group.Columns)
		if err != nil {
			return out, err
		}
		out.Groups[i].Hits = hits
	}
	for i, merged := range found.Merged {
		at, hit := int(merged.Group), int(merged.Hit)
		if at >= len(out.Groups) || hit >= len(out.Groups[at].Hits) {
			return out, core.Errorf(core.Protocol, "merged names hit %d of group %d, past its hits", hit, at)
		}
		out.Merged[i] = Merged{Index: out.Groups[at].Index, Group: at, Hit: &out.Groups[at].Hits[hit]}
	}
	return out, nil
}
