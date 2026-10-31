package search_test

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"testing"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/ways"
	"tinyshed.org/tinystore/limits"
	"tinyshed.org/tinystore/search"
)

type named struct {
	Name string `json:"name"`
}

// limitOf is what a limit error says of its bound: its name, what the call
// wanted and the bound.
func limitOf(t *testing.T, err error) (string, int64, int64) {
	t.Helper()
	var e *tinystore.Error
	if !errors.As(err, &e) || !errors.Is(err, tinystore.ErrLimit) {
		t.Fatalf("got %v, want a limit", err)
	}
	return e.Limit, e.Wanted, e.Bound
}

// contract: ERR-05
func TestACallPastABoundNamesItWhatItWantedAndTheBound(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		courses := ways.Must(search.OpenIndex[named](store, "courses", search.Fields{"name": 1}))
		many := make([]search.Doc[named], 1001)
		for n := range many {
			many[n] = search.Doc[named]{ID: fmt.Sprint(n), Value: named{fmt.Sprintf("course %d", n)}}
		}
		err := courses.SetMany(ctx, many)
		if said := "index courses: a set_many of 1001 documents, past 1000: limit"; err == nil || err.Error() != said {
			t.Fatalf("got %v, want %s", err, said)
		}
		if limit, wanted, bound := limitOf(t, err); limit != limits.SetManyDocuments.Name || wanted != 1001 || bound != 1000 {
			t.Fatalf("set_many %q %d %d", limit, wanted, bound)
		}
		_, err = courses.Search(ctx, "course", search.Limit(2000))
		if limit, wanted, bound := limitOf(t, err); limit != limits.SearchHits.Name || wanted != 2000 || bound != 1000 {
			t.Fatalf("search %q %d %d", limit, wanted, bound)
		}
	})
}

// contract: ERR-05
func TestARequestPastOneBodyFailsBeforeItIsSent(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		catalogue := ways.Must(search.OpenIndex[named](store, "catalogue", search.Fields{"name": 1}))
		long := make([]search.Doc[named], 300)
		for n := range long {
			long[n] = search.Doc[named]{ID: fmt.Sprint(n), Value: named{strings.Repeat(fmt.Sprintf("course %d ", n), 1000)}}
		}
		err := catalogue.SetMany(ctx, long)
		limit, wanted, bound := limitOf(t, err)
		if limit != limits.RequestBytes.Name || bound != limits.RequestBytes.Bound || wanted <= bound {
			t.Fatalf("request %q %d %d", limit, wanted, bound)
		}
		if said := fmt.Sprintf("search.setMany: a request of %d bytes, past the %d a body holds", wanted, bound); err.Error() != said {
			t.Fatalf("got %v, want %s", err, said)
		}
		if _, found := ways.Must2(catalogue.Get(ctx, "0")); found {
			t.Fatal("a refused request set a document")
		}
	})
}
