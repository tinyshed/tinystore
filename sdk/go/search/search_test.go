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

// contract: SRCH-12
func TestAWordTypedWithTheOtherLayoutOnIsReadAsItsKeysTypeThereAndCorrectedSaysSo(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		courses := ways.Must(search.OpenIndex[course](store, "catalogue", search.Fields{"name": 1}, search.Layouts("ru", "en")))
		ways.Must(0, courses.SetMany(ctx, []search.Doc[course]{
			{ID: "1", Value: course{"Криптографические методы защиты информации"}},
			{ID: "2", Value: course{"Линейная алгебра"}},
			{ID: "3", Value: course{"Python для анализа данных"}},
			{ID: "4", Value: course{"Инженерная графика"}},
		}))
		for _, asked := range []struct {
			text, corrected string
			want            []string
		}{
			{"rhbgnj", "крипто", []string{"1"}},
			{"kby fku", "kby алг", []string{"2"}},
			{"by; uhfa", "by; граф", []string{"4"}},
			{"знерщт", "python", []string{"3"}},
			{"python", "", []string{"3"}},
		} {
			result := ways.Must(courses.Search(ctx, asked.text, search.Prefix()))
			if got := ids(result); !reflect.DeepEqual(got, asked.want) || result.Corrected != asked.corrected {
				t.Fatalf("%q: %v %q, want %v %q", asked.text, got, result.Corrected, asked.want, asked.corrected)
			}
		}
	})
}

// contract: SRCH-13
func TestPrefixEveryReadsEachWordAsAStartADocumentHoldingOneForEachFirst(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		courses := ways.Must(search.OpenIndex[course](store, "shortened", search.Fields{"name": 1}))
		ways.Must(0, courses.SetMany(ctx, []search.Doc[course]{
			{ID: "1", Value: course{"Линейная алгебра"}},
			{ID: "2", Value: course{"Алгоритмизация и программирование"}},
			{ID: "3", Value: course{"Проектирование технологических линий"}},
		}))
		every, last := []search.QueryOption{search.PrefixEvery()}, []search.QueryOption{search.Prefix()}
		for _, asked := range []struct {
			text string
			opts []search.QueryOption
			want []string
		}{
			{"лин алг", every, []string{"1", "3", "2"}},
			{"алг лин", every, []string{"1", "3", "2"}},
			{"лин алгебра", every, []string{"1", "3"}},
			{"линейная алг", every, []string{"1", "2"}},
			{"лин алг", last, []string{"3", "1", "2"}},
		} {
			if got := ids(ways.Must(courses.Search(ctx, asked.text, asked.opts...))); !reflect.DeepEqual(got, asked.want) {
				t.Fatalf("%q: %v, want %v", asked.text, got, asked.want)
			}
		}
	})
}

type listed struct {
	Name  string   `json:"name"`
	Short string   `json:"short"`
	Tags  []string `json:"tags,omitempty"`
}

// contract: SRCH-14
func TestAHitSaysWhereItMatchedInBytesThatSliceItsOwnStrings(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		catalog := ways.Must(search.OpenIndex[listed](store, "catalog", search.Fields{"name": 2, "short": 1, "tags": 1}))
		ways.Must(0, catalog.SetMany(ctx, []search.Doc[listed]{
			{ID: "1", Value: listed{Name: "Линейная алгебра и аналитическая геометрия", Short: "ЛААГ"}},
			{ID: "2", Value: listed{Name: "Криптографические методы защиты информации", Short: "КМЗИ"}},
			{ID: "3", Value: listed{Name: "🦀 Ёлка", Tags: []string{"ёлки", "пальмы"}}},
		}))
		sliced := func(value listed, m search.Match) string {
			text := map[string]string{"name": value.Name, "short": value.Short}[m.Field]
			if m.Field == "tags" {
				text = value.Tags[m.Item]
			}
			return text[m.Start:m.End]
		}
		shown := func(text string) [][4]string {
			out := [][4]string{}
			for _, hit := range ways.Must(catalog.Search(ctx, text, search.Prefix(), search.Matches())).Hits {
				for _, m := range hit.Matches {
					out = append(out, [4]string{hit.ID, m.Field, sliced(hit.Value, m), m.Word})
				}
			}
			return out
		}
		for _, asked := range []struct {
			text string
			want [][4]string
		}{
			{"алгебре", [][4]string{{"1", "name", "алгебра", "алгебре"}}},
			{"крипто", [][4]string{{"2", "name", "Крипто", "крипто"}}},
			{"криптографичесике методы", [][4]string{
				{"2", "name", "Криптографические", "криптографичесике"},
				{"2", "name", "методы", "методы"},
			}},
		} {
			if got := shown(asked.text); !reflect.DeepEqual(got, asked.want) {
				t.Fatalf("%q: %v, want %v", asked.text, got, asked.want)
			}
		}
		// the crab before Ёлка is four bytes
		tree := ways.Must(catalog.Search(ctx, "елка", search.Matches())).Hits[0]
		want := []search.Match{
			{Field: "name", Start: 5, End: 13, Word: "елка"},
			{Field: "tags", Item: 0, Start: 0, End: 8, Word: "елка"},
		}
		if !reflect.DeepEqual(tree.Matches, want) {
			t.Fatalf("matches %v, want %v", tree.Matches, want)
		}
		if plain := ways.Must(catalog.Search(ctx, "елка")).Hits[0]; plain.Matches != nil {
			t.Fatalf("matches %v unasked", plain.Matches)
		}
	})
}

func TestAHitKeepsEachHalfOfItsScoreAndAFloorKeepsAFarVectorOutOfTheSum(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		opinions := ways.Must(search.OpenIndex[product](store, "opinions", search.FieldNames("title"), search.Vectors{Dimensions: 3}))
		ways.Must(0, opinions.Set(ctx, "dota", product{Title: "dota is dead"}, search.Vector([]float32{1, 0, 0})))
		ways.Must(0, opinions.Set(ctx, "rain", product{Title: "loves the rain"}, search.Vector([]float32{0, 1, 0})))
		ways.Must(0, opinions.Set(ctx, "patch", product{Title: "the new dota patch"}, search.Vector([]float32{0, 0, 1})))
		far := search.Vector([]float32{-1, -1, -1})
		summed := ways.Must(opinions.Search(ctx, "dota", far))
		for _, hit := range summed.Hits {
			if hit.ID != "rain" {
				continue
			}
			if hit.Score != 0.5 || hit.TextScore != nil || hit.Similarity == nil || math.Abs(*hit.Similarity+1/math.Sqrt(3)) > 1e-6 {
				t.Fatalf("rain %+v", hit)
			}
		}
		floored := ways.Must(opinions.Search(ctx, "dota", far, search.MinSimilarity(0.58)))
		if !reflect.DeepEqual(ids(floored), []string{"dota", "patch"}) || floored.Hits[1].TextScore == nil {
			t.Fatalf("floored %+v", floored)
		}
		_, err := opinions.Search(ctx, "dota", far, search.MinSimilarity(0.58), search.Hybrid{Rerank: true})
		is(t, err, tinystore.ErrInvalid)
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
