package search_test

import (
	"context"
	"encoding/binary"
	"errors"
	"math"
	"reflect"
	"testing"
	"testing/fstest"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/ways"
	"tinyshed.org/tinystore/search"
	"tinyshed.org/tinystore/sqldb"
)

type article struct {
	Title string `json:"title"`
	Body  string `json:"body"`
}

type product struct {
	Title    string   `json:"title"`
	Category string   `json:"category,omitempty"`
	Price    *int     `json:"price,omitempty"`
	Tags     []string `json:"tags,omitempty"`
}

func is(t *testing.T, err error, kind error) {
	t.Helper()
	if !errors.Is(err, kind) {
		t.Fatalf("got %v, want %v", err, kind)
	}
}

func ids[T any](result search.Result[T]) []string {
	out := []string{}
	for _, hit := range result.Hits {
		out = append(out, hit.ID)
	}
	return out
}

func TestDocumentsSetByIDAreFoundByAnyOfTheirWordsTheBestFirst(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		articles := ways.Must(search.OpenIndex[article](store, "articles", search.Fields{"title": 2, "body": 1}))
		ways.Must(0, articles.Set(ctx, "a", article{"Sourdough bread", "flour, water and salt"}))
		ways.Must(0, articles.SetMany(ctx, []search.Doc[article]{
			{ID: "b", Value: article{"Rye", "bread from rye flour"}},
			{ID: "c", Value: article{"Butter", "milk and cream"}},
		}))
		result := ways.Must(articles.Search(ctx, "bread salt"))
		if !reflect.DeepEqual(ids(result), []string{"a", "b"}) || result.Hits[0].Value != (article{"Sourdough bread", "flour, water and salt"}) {
			t.Fatalf("result %+v", result)
		}
		if page := ways.Must(articles.Search(ctx, "flour", search.Limit(1), search.Offset(1))); len(page.Hits) != 1 {
			t.Fatalf("page %+v", page)
		}
		if corrected := ways.Must(articles.Search(ctx, "Sourdouhg")).Corrected; corrected != "sourdough" {
			t.Fatalf("corrected %q", corrected)
		}
		if got, found := ways.Must2(articles.Get(ctx, "c")); !found || got != (article{"Butter", "milk and cream"}) {
			t.Fatalf("get %+v %v", got, found)
		}
		if !ways.Must(articles.Delete(ctx, "c")) || ways.Must(articles.Delete(ctx, "c")) {
			t.Fatal("delete says whether there was one")
		}
		if _, found := ways.Must2(articles.Get(ctx, "c")); found {
			t.Fatal("a deleted document is found")
		}
	})
}

type course struct {
	Name string `json:"name"`
}

// contract: SRCH-11
func TestAWordTypedHalfwayFindsTheWordsItBeginsWithPrefixAndNothingWithout(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		courses := ways.Must(search.OpenIndex[course](store, "courses", search.Fields{"name": 1}))
		ways.Must(0, courses.SetMany(ctx, []search.Doc[course]{
			{ID: "1", Value: course{"Cryptographic methods of information security"}},
			{ID: "2", Value: course{"Cryptanalysis"}},
			{ID: "3", Value: course{"Cryptography"}},
			{ID: "4", Value: course{"Foundations of cryptography"}},
		}))
		for _, asked := range []struct {
			text string
			opts []search.QueryOption
			want []string
		}{
			{"crypt", nil, []string{}},
			{"crypt", []search.QueryOption{search.Prefix()}, []string{"2", "3", "4", "1"}},
			{"cryptograph", []search.QueryOption{search.Prefix()}, []string{"3", "4", "1"}},
			{"crypt ", []search.QueryOption{search.Prefix()}, []string{}},
		} {
			if got := ids(ways.Must(courses.Search(ctx, asked.text, asked.opts...))); !reflect.DeepEqual(got, asked.want) {
				t.Fatalf("%q %d options: %v, want %v", asked.text, len(asked.opts), got, asked.want)
			}
		}
	})
}

func TestAVectorFindsTheNearestByItsCosineAndWithWordsBothAreSummedOrReranked(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		shoes := ways.Must(search.OpenIndex[product](store, "shoes", search.FieldNames("title"),
			search.Filters("category", "price", "tags"), search.Vectors{Dimensions: 3}))
		fifty, hundredFifty := 50, 150
		ways.Must(0, shoes.Set(ctx, "a", product{"red shoes", "shoes", &fifty, []string{"sale"}}, search.Vector([]float32{1, 0, 0})))
		ways.Must(0, shoes.Set(ctx, "b", product{Title: "blue shoes", Category: "boots", Price: &hundredFifty}, search.Vector([]float32{0, 1, 0})))
		ways.Must(0, shoes.Set(ctx, "c", product{Title: "red hat"}, search.NoVector()))
		if got := ids(ways.Must(shoes.SearchVector(ctx, []float32{1, 0, 0}))); !reflect.DeepEqual(got, []string{"a", "b"}) {
			t.Fatalf("by vector %v", got)
		}
		reranked := ways.Must(shoes.Search(ctx, "red", search.Vector([]float32{0, 1, 0}), search.Hybrid{Rerank: true}))
		if !reflect.DeepEqual(ids(reranked), []string{"a", "c"}) || reranked.Hits[1].Score != -1 {
			t.Fatalf("reranked %+v", reranked)
		}
		cheap := ways.Must(shoes.Search(ctx, "shoes", search.Where{"category": []string{"shoes", "boots"}, "price": search.Range{Lt: 100}}))
		if !reflect.DeepEqual(ids(cheap), []string{"a"}) {
			t.Fatalf("cheap %v", ids(cheap))
		}
		if got := ids(ways.Must(shoes.Search(ctx, "red", search.Where{"category": nil}))); !reflect.DeepEqual(got, []string{"c"}) {
			t.Fatalf("no category %v", got)
		}
		either := ways.Must(shoes.Search(ctx, "shoes", search.Or(search.Where{"tags": "sale"}, search.Where{"price": search.Range{Gte: 100}})))
		if !reflect.DeepEqual(ids(either), []string{"a", "b"}) {
			t.Fatalf("either %v", ids(either))
		}
		is(t, shoes.Set(ctx, "d", product{Title: "no vector said"}), tinystore.ErrInvalid)
		_, err := shoes.Search(ctx, "red", search.Where{"price": math.NaN()})
		is(t, err, tinystore.ErrInvalid)
	})
}

func TestAFieldAddedToAnIndexThatHoldsDocumentsIsReadBesideItUntilItSwitchesIn(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		notes := ways.Must(search.OpenIndex[article](store, "notes", search.FieldNames("title")))
		ways.Must(0, notes.Set(ctx, "n", article{"bread", "salt"}))
		both := ways.Must(search.OpenIndex[article](store, "notes", search.FieldNames("title", "body")))
		ways.Must(0, both.Ready(ctx))
		if status := ways.Must(both.Status(ctx)); status != (search.Status{State: search.Ready}) {
			t.Fatalf("status %+v", status)
		}
		if got := ids(ways.Must(both.Search(ctx, "salt"))); !reflect.DeepEqual(got, []string{"n"}) {
			t.Fatalf("salt %v", got)
		}
		manual := ways.Must(search.OpenIndex[any](store, "notes", search.FieldNames("title"), search.Rebuild(search.Manual)))
		if status := ways.Must(manual.Status(ctx)); status.State != search.Ready {
			t.Fatalf("manual %+v", status)
		}
		refused := ways.Must(search.OpenIndex[any](store, "notes", search.FieldNames("title", "body", "tags"), search.Rebuild(search.Manual)))
		_, err := refused.Status(ctx)
		is(t, err, tinystore.ErrInvalid)
		ways.Must(0, search.Reindex(ctx, store, "notes", search.FieldNames("title", "body")))
		reindexed := ways.Must(search.OpenIndex[any](store, "notes", search.FieldNames("title", "body"), search.Rebuild(search.Manual)))
		ways.Must(0, reindexed.Ready(ctx))
	})
}

type row struct {
	ID        int    `db:"id"`
	Title     string `db:"title"`
	Done      int    `db:"done"`
	Embedding []byte `db:"embedding"`
}

func TestATablesIndexFindsItsRowsEachHitItsRowAndFollowsEveryWriteToTheTable(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		db := ways.Must(sqldb.Open(ctx, store, "shop", sqldb.Migrations(fstest.MapFS{"0001_articles.sql": {Data: []byte(
			"create table articles (id integer primary key, title text not null, done integer not null default 0, embedding blob)")}})))
		ways.Must(db.Exec(ctx, "insert into articles (id, title) values (1, 'Sourdough bread')"))
		articles := ways.Must(search.OpenIndex[row](db, "articles_search", search.Table("articles"), search.FieldNames("title"),
			search.Filters("done"), search.Vectors{Column: "embedding", Dimensions: 3}))
		ways.Must(0, articles.Ready(ctx))
		if got := ids(ways.Must(articles.Search(ctx, "bread"))); !reflect.DeepEqual(got, []string{"1"}) {
			t.Fatalf("bread %v", got)
		}
		embedding := make([]byte, 12)
		binary.LittleEndian.PutUint32(embedding, math.Float32bits(1))
		ways.Must(db.Exec(ctx, "insert into articles (id, title, done, embedding) values (2, 'Rye bread', 1, ?)", embedding))
		done := ways.Must(articles.Search(ctx, "bread", search.Where{"done": true}))
		if !reflect.DeepEqual(ids(done), []string{"2"}) || done.Hits[0].Value.Title != "Rye bread" {
			t.Fatalf("done %+v", done)
		}
		if got := ids(ways.Must(articles.SearchVector(ctx, []float32{1, 0, 0}))); !reflect.DeepEqual(got, []string{"2"}) {
			t.Fatalf("by vector %v", got)
		}
		_, err := db.Exec(ctx, "insert into articles (id, title, embedding) values (3, 'ragged', ?)", make([]byte, 5))
		is(t, err, tinystore.ErrInvalid)
		if !ways.Must(search.Drop(ctx, db, "articles_search")) {
			t.Fatal("drop says whether there was one")
		}
		_, err = articles.Search(ctx, "bread")
		is(t, err, tinystore.ErrInvalid)
	})
}

func TestAnIndexIsListedAndDroppedWithEveryDocumentItHolds(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		posts := ways.Must(search.OpenIndex[string](store, "posts"))
		ways.Must(0, posts.Set(ctx, "p", "hello"))
		var post *search.Listed
		for _, index := range ways.Must(search.List(ctx, store)) {
			if index.Name == "posts" {
				post = &index
			}
		}
		if post == nil || post.Documents != 1 || post.Opened.IsZero() {
			t.Fatalf("listed %+v", post)
		}
		if !ways.Must(search.Drop(ctx, store, "posts")) || ways.Must(search.Drop(ctx, store, "posts")) {
			t.Fatal("drop says whether there was one")
		}
		_, err := posts.Search(ctx, "hello")
		is(t, err, tinystore.ErrInvalid)
	})
}
