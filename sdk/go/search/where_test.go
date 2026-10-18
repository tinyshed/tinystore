package search

import (
	"math"
	"testing"

	"tinyshed.org/tinystore/internal/wire"
)

func TestAWhereIsWrittenAsTheWireTakesIt(t *testing.T) {
	written := func(option QueryOption) string {
		var q wire.SearchQuery
		option.search(&q)
		return *q.Where
	}
	if got := written(Where{"a": 1, "b": nil, "c": []string{"x", "y"}, "d": Range{Gte: 2}}); got !=
		`[{"eq":["a",1]},{"missing":"b"},{"in":["c",["x","y"]]},{"range":["d",{"gte":2}]}]` {
		t.Fatalf("where %s", got)
	}
	if got := written(Or(Where{"a": 1}, Where{"b": true})); got != `[{"or":[{"and":[{"eq":["a",1]}]},{"and":[{"eq":["b",true]}]}]}]` {
		t.Fatalf("or %s", got)
	}
	if got := written(Where{"a": math.NaN()}); got != "" {
		t.Fatalf("a NaN wrote %s", got)
	}
}
