package metrics

import (
	"errors"
	"slices"
	"strings"
	"testing"
	"time"
)

// conditionSeries is a crowd the conditions below choose among: two hosts of
// api and one of web, statuses 200 to 502, one series without env.
func conditionSeries(t *testing.T) *Store {
	t.Helper()
	store, _ := openTestStore(t, Options{Retention: time.Hour})
	crowd := []Labels{
		{"host": "api-1", "status": "200", "env": "prod"},
		{"host": "api-2", "status": "500", "env": "prod"},
		{"host": "api-2", "status": "502", "env": "dev"},
		{"host": "web-1", "status": "500", "env": "prod"},
		{"host": "web-1", "status": "404"},
		{"host": "apiary", "status": "502", "env": "prod"},
	}
	for _, labels := range crowd {
		series := Series{Name: "http_requests_total", Kind: Counter, Labels: labels}
		if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: []Sample{{At: testEpoch, Value: 1}}}}); err != nil {
			t.Fatal(err)
		}
	}
	return store
}

func readHosts(t *testing.T, store *Store, request Range) []string {
	t.Helper()
	request.From, request.To = testEpoch, testEpoch+1
	results, err := store.Read(t.Context(), request)
	if err != nil {
		t.Fatal(err)
	}
	found := make([]string, 0, len(results))
	for _, result := range results {
		found = append(found, result.Series.Labels["host"]+"/"+result.Series.Labels["status"])
	}
	slices.Sort(found)
	return found
}

func TestConditionsFindSeriesBeyondEquality(t *testing.T) {
	store := conditionSeries(t)
	name := "http_requests_total"
	for _, check := range []struct {
		where Where
		match Labels
		want  string
	}{
		{where: Where{"status": OneOf("500", "502")}, want: "api-2/500 api-2/502 apiary/502 web-1/500"},
		{where: Where{"env": NoneOf("dev")}, want: "api-1/200 api-2/500 apiary/502 web-1/404 web-1/500"},
		{where: Where{"env": NoneOf("dev", "prod")}, want: "web-1/404"},
		{where: Where{"host": Prefix("api-")}, want: "api-1/200 api-2/500 api-2/502"},
		{
			where: Where{"host": Prefix("api"), "status": OneOf("502", "200"), "env": NoneOf("dev")},
			want:  "api-1/200 apiary/502",
		},
		{where: Where{"status": OneOf("500")}, match: Labels{"env": "prod"}, want: "api-2/500 web-1/500"},
		{where: Where{"status": OneOf("418", "503")}, want: ""},
		{where: Where{"host": Prefix("db-")}, want: ""},
		{where: Where{"env": NoneOf("staging")}, want: "api-1/200 api-2/500 api-2/502 apiary/502 web-1/404 web-1/500"},
	} {
		got := strings.Join(readHosts(t, store, Range{Name: name, Match: check.match, Where: check.where}), " ")
		if got != check.want {
			t.Errorf("%v %v: found %q, want %q", check.match, check.where, got, check.want)
		}
	}

	// a OneOf or a Prefix finds series without a name to start from
	if got := strings.Join(readHosts(t, store, Range{Where: Where{"host": Prefix("web")}}), " "); got != "web-1/404 web-1/500" {
		t.Errorf("a Prefix alone found %q", got)
	}
}

func TestAConditionThatFindsNothingOrCannotBeTakenIsRefused(t *testing.T) {
	store := conditionSeries(t)
	for _, request := range []Range{
		{Where: Where{"env": NoneOf("dev")}},
		{Name: "http_requests_total", Where: Where{"env": {}}},
		{Name: "http_requests_total", Where: Where{"env": OneOf()}},
		{Name: "http_requests_total", Where: Where{"env": NoneOf()}},
		{Name: "http_requests_total", Where: Where{"host": Prefix("")}},
		{Name: "http_requests_total", Where: Where{"__name__": OneOf("cpu")}},
		{Name: "http_requests_total", Where: Where{"": OneOf("x")}},
		{Name: "http_requests_total", Match: Labels{"env": "prod"}, Where: Where{"env": NoneOf("dev")}},
		{Name: "http_requests_total", Where: Where{"env": OneOf("\xff")}},
	} {
		request.From, request.To = testEpoch, testEpoch+1
		if _, err := store.Read(t.Context(), request); !errors.Is(err, ErrInvalid) {
			t.Errorf("%+v: %v, want ErrInvalid", request.Where, err)
		}
	}
}

func TestAPrefixEndsWhereNoValueBeginsWithIt(t *testing.T) {
	if end := prefixEnd("api-"); end != "api." {
		t.Fatalf(`prefixEnd("api-") = %q, want "api."`, end)
	}
	for _, value := range []string{"api-", "api-1", "api-\U0010ffff"} {
		if value < "api-" || value >= prefixEnd("api-") {
			t.Errorf("%q begins with api- and falls outside its range", value)
		}
	}
	for _, value := range []string{"api", "api.", "apj", "api.-"} {
		if value >= "api-" && value < prefixEnd("api-") {
			t.Errorf("%q does not begin with api- and falls inside its range", value)
		}
	}
}
