package sqldb

import (
	"context"
	"encoding/json"
	"iter"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"unicode"
)

// Every query of testdata/sql/queries.json, built with this SDK's builder
// from its steps: the text and the values it sends, which every SDK's builder
// must write alike. A runner that records what it is given stands for a database.

type recorder struct{ last *Piece }

func (r *recorder) rows(_ context.Context, statement Piece, _ string) (answered, error) {
	r.last = &statement
	return answered{}, statement.err
}

func (r *recorder) exec(_ context.Context, statement Piece) (Result, error) {
	r.last = &statement
	return Result{}, statement.err
}

func (r *recorder) each(context.Context, Piece) iter.Seq2[[]any, error] {
	return func(func([]any, error) bool) {}
}

func (r *recorder) describeIt() string { return "sql vectors" }

type vector struct {
	Name   string            `json:"name"`
	Table  string            `json:"table"`
	Steps  []json.RawMessage `json:"steps"`
	Update [][]any           `json:"update"`
	Delete bool              `json:"delete"`
	Insert [][][]any         `json:"insert"`
	Upsert []json.RawMessage `json:"upsert"`
	Count  bool              `json:"count"`
	Text   string            `json:"text"`
	Values []any             `json:"values"`
}

func conditionOf(t *testing.T, given map[string]any) Condition {
	t.Helper()
	switch {
	case given["equal"] != nil:
		eq := Eq{}
		for name, value := range given["equal"].(map[string]any) {
			eq[name] = value
		}
		return eq
	case given["sql"] != nil:
		items := given["sql"].([]any)
		return SQL(items[0].(string), items[1:]...)
	case given["or"] != nil:
		return Or(conditionsOf(t, given["or"])...)
	case given["and"] != nil:
		return And(conditionsOf(t, given["and"])...)
	case given["has"] != nil:
		items := given["has"].([]any)
		return Has(items[0].(string), items[1])
	}
	t.Fatalf("no condition %v", given)
	return nil
}

func conditionsOf(t *testing.T, given any) []Condition {
	var out []Condition
	for _, each := range given.([]any) {
		out = append(out, conditionOf(t, each.(map[string]any)))
	}
	return out
}

func applied(t *testing.T, q *Query[Row], step json.RawMessage) *Query[Row] {
	t.Helper()
	var one map[string]any
	if err := json.Unmarshal(step, &one); err != nil {
		t.Fatal(err)
	}
	for verb, argument := range one {
		switch verb {
		case "where":
			return q.Where(conditionOf(t, argument.(map[string]any)))
		case "having":
			return q.Having(conditionOf(t, argument.(map[string]any)))
		case "select":
			return Select[Row](q, argument.(string))
		case "join", "leftJoin":
			items := argument.([]any)
			var on any = items[1]
			if m, ok := items[1].(map[string]any); ok {
				on = conditionOf(t, m).condition()
			}
			if verb == "join" {
				return q.Join(items[0].(string), on)
			}
			return q.LeftJoin(items[0].(string), on)
		case "groupBy":
			var columns []string
			for _, c := range argument.([]any) {
				columns = append(columns, c.(string))
			}
			return q.GroupBy(columns...)
		case "orderBy":
			items := argument.([]any)
			return q.OrderBy(items[0].(string), Direction(items[1].(string)))
		case "limit":
			return q.Limit(int(argument.(float64)))
		case "offset":
			return q.Offset(int(argument.(float64)))
		case "include":
			items := argument.([]any)
			inner := items[1].(map[string]any)
			query := OpenTable[Row](&recorder{}, inner["table"].(string)).Query
			for _, each := range inner["steps"].([]any) {
				raw, _ := json.Marshal(each)
				query = applied(t, query, raw)
			}
			return q.Include(items[0].(string), query)
		}
		t.Fatalf("no step %s", verb)
	}
	return q
}

// orderedRow is a row whose columns keep the vector's order: a struct made for it.
func orderedRow(pairs [][]any) any {
	fields := make([]reflect.StructField, len(pairs))
	for i, pair := range pairs {
		name := pair[0].(string)
		fields[i] = reflect.StructField{
			Name: "F" + strings.Map(func(r rune) rune {
				if unicode.IsLetter(r) || unicode.IsDigit(r) {
					return r
				}
				return '_'
			}, name),
			Type: reflect.TypeFor[any](),
			Tag:  reflect.StructTag(`db:"` + name + `"`),
		}
	}
	row := reflect.New(reflect.StructOf(fields)).Elem()
	for i, pair := range pairs {
		if pair[1] != nil {
			row.Field(i).Set(reflect.ValueOf(pair[1]))
		}
	}
	return row.Interface()
}

// keptValue is a value as SQLite keeps it, which is what the vectors say: a bool
// 0 or 1, a number as JSON reads it.
func keptValue(value any) any {
	switch v := value.(type) {
	case bool:
		if v {
			return float64(1)
		}
		return float64(0)
	case int64:
		return float64(v)
	case int:
		return float64(v)
	}
	return value
}

func TestEveryQueryVectorIsWrittenAlike(t *testing.T) {
	text, err := os.ReadFile(filepath.Join("..", "..", "..", "testdata", "sql", "queries.json"))
	if err != nil {
		t.Fatal(err)
	}
	var file struct {
		Queries []vector `json:"queries"`
	}
	if err := json.Unmarshal(text, &file); err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	for _, v := range file.Queries {
		t.Run(v.Name, func(t *testing.T) {
			r := &recorder{}
			table := OpenTable[Row](r, v.Table)
			q := table.Query
			for _, step := range v.Steps {
				q = applied(t, q, step)
			}
			switch {
			case v.Update != nil:
				set := Set{}
				for _, pair := range v.Update {
					if m, ok := pair[1].(map[string]any); ok {
						set[pair[0].(string)] = conditionOf(t, m).condition()
					} else {
						set[pair[0].(string)] = pair[1]
					}
				}
				_, err = q.Update(ctx, set)
			case v.Delete:
				_, err = q.Delete(ctx)
			case v.Insert != nil:
				var rows []any
				for _, row := range v.Insert {
					rows = append(rows, orderedRow(row))
				}
				statement := insertOf(table.name, rows)
				_, err = r.rows(ctx, Piece{text: statement.text + " returning *", values: statement.values, err: statement.err}, "all")
			case v.Upsert != nil:
				var pairs [][]any
				var options map[string]any
				_ = json.Unmarshal(v.Upsert[0], &pairs)
				_ = json.Unmarshal(v.Upsert[1], &options)
				opts := []UpsertOption{Conflict(options["conflict"].(string))}
				var update []string
				for _, u := range options["update"].([]any) {
					update = append(update, u.(string))
				}
				opts = append(opts, Update(update...))
				if where, ok := options["where"].(map[string]any); ok {
					opts = append(opts, Where(conditionOf(t, where)))
				}
				anyTable := OpenTable[any](r, v.Table)
				_, err = anyTable.Upsert(ctx, orderedRow(pairs), opts...)
			case v.Count:
				_, err = q.Count(ctx)
			}
			if err != nil {
				t.Fatal(err)
			}
			gotText, gotValues := "", []any(nil)
			if r.last != nil {
				gotText, gotValues = r.last.text, r.last.values
			} else {
				gotText, gotValues, err = q.SQL()
				if err != nil {
					t.Fatal(err)
				}
			}
			if gotText != v.Text {
				t.Fatalf("text\n got %s\nwant %s", gotText, v.Text)
			}
			if len(gotValues) != len(v.Values) {
				t.Fatalf("values %v, want %v", gotValues, v.Values)
			}
			for i := range gotValues {
				if !reflect.DeepEqual(keptValue(gotValues[i]), v.Values[i]) {
					t.Fatalf("value %d: %#v, want %#v", i, gotValues[i], v.Values[i])
				}
			}
		})
	}
}

func TestIncludedRowsReadFromTheirLiterals(t *testing.T) {
	rows, err := literalRows("(1,'it''s',NULL),(2,X'00ff',1.5),(3,T X'6100',-2)")
	if err == nil {
		t.Fatalf("a T before a space is no literal, read as %v", rows)
	}
	rows, err = literalRows("(1,'it''s',NULL),(2,X'00ff',1.5),(3,TX'6100',-2)")
	if err != nil {
		t.Fatal(err)
	}
	want := [][]any{{int64(1), "it's", nil}, {int64(2), []byte{0, 255}, 1.5}, {int64(3), "a\x00", int64(-2)}}
	if !reflect.DeepEqual(rows, want) {
		t.Fatalf("rows %#v", rows)
	}
	names, err := namesOf(`p.id, p.title, upper(o.status) as "Status"`, "the included")
	if err != nil || strings.Join(names, ",") != "id,title,Status" {
		t.Fatalf("names %v %v", names, err)
	}
	if _, err := namesOf("count(*)", "the included"); err == nil {
		t.Fatal("a column with no name is refused")
	}
}

func TestAFieldReadsTheColumnOfItsNameInSnakeCase(t *testing.T) {
	for field, column := range map[string]string{"AuthorID": "author_id", "CreatedAt": "created_at", "ID": "id", "TS": "ts", "CPU": "cpu", "HTTPServer": "http_server", "V2Api": "v2_api"} {
		if got := snake(field); got != column {
			t.Errorf("%s reads %s, not %s", field, got, column)
		}
	}
}
