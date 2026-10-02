package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"maps"
	"slices"
	"strings"
	"testing"
)

func TestDigestNeverAliasesDifferentLabels(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	first := testSeries()
	second := testSeries()
	second.Labels["host"] = "another"
	if err := store.Ingest(t.Context(), []Batch{{Series: first, Samples: testSamples(1)}}); err != nil {
		t.Fatal(err)
	}
	_, canonical, err := canonicalLabels(keptOf(t, second), true)
	if err != nil {
		t.Fatal(err)
	}
	if err = store.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, writeErr := tx.ExecContext(t.Context(), `update series set identity=? where id=1`, seriesIdentity(canonical))
		return writeErr
	}); err != nil {
		t.Fatal(err)
	}
	if err = store.Ingest(t.Context(), []Batch{{Series: second, Samples: testSamples(1)}}); !errors.Is(err, ErrConflict) {
		t.Fatalf("collision accepted: %v", err)
	}
}

func TestRepeatedLabelsShareDictionaryEntries(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	first := testSeries()
	second := testSeries()
	second.Labels["host"] = "two"
	if err := store.Ingest(t.Context(), []Batch{{Series: first, Samples: testSamples(1)}, {Series: second, Samples: testSamples(1)}}); err != nil {
		t.Fatal(err)
	}
	if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
		var labels, postings int
		readErr := tx.QueryRowContext(t.Context(), `select (select count(*) from label_values),(select count(*) from postings)`).Scan(&labels, &postings)
		if labels != 3 || postings != 4 {
			t.Fatalf("labels=%d postings=%d", labels, postings)
		}
		return readErr
	}); err != nil {
		t.Fatal(err)
	}
	for _, series := range []Series{first, second} {
		result, err := store.Read(t.Context(), Range{Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + 1})
		if err != nil || len(result) != 1 {
			t.Fatal("dictionary matching", err, result)
		}
	}
}

func TestContainerSeriesKeepsEveryLabelThroughTheDictionary(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	series := Series{Name: "docker_container_mem_usage", Kind: Gauge, Labels: Labels{}}
	for _, name := range []string{
		"architecture", "build-date", "com.docker.compose.config-hash", "com.docker.compose.container-number",
		"com.docker.compose.image", "com.docker.compose.oneoff", "com.docker.compose.project",
		"com.docker.compose.project.config_files", "com.docker.compose.project.environment_file",
		"com.docker.compose.project.working_dir", "com.docker.compose.service", "com.docker.compose.version",
		"com.redhat.component", "com.redhat.license_terms", "container_image", "container_name",
		"container_version", "description", "distribution-scope", "engine_host", "host",
		"io.buildah.version", "io.k8s.description", "io.k8s.display-name", "maintainer", "name",
		"release", "server_version", "summary", "url", "vcs-ref", "vcs-type", "vendor", "version",
		"deep", "deeper", "deepest",
	} {
		series.Labels[name] = name + "-value"
	}
	if len(series.Labels) != 37 {
		t.Fatalf("fixture carries %d labels", len(series.Labels))
	}
	points := testSamples(4)
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	result, err := store.Read(t.Context(), Range{Name: series.Name, Match: series.Labels, From: testEpoch, To: testEpoch + int64(len(points))})
	if err != nil || len(result) != 1 {
		t.Fatal("container series is unreadable", err, len(result))
	}
	if got := result[0].Series; got.Name != series.Name || !maps.Equal(got.Labels, series.Labels) {
		t.Fatalf("the series came back as %s %v", got.Name, got.Labels)
	}
	assertSamples(t, result[0].Samples, points)
}

func TestRegistryStoresIdentifiersRatherThanLabelText(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: testSamples(1)}}); err != nil {
		t.Fatal(err)
	}
	if err := store.file.View(t.Context(), func(tx *sql.Tx) error {
		var identity string
		var ids []byte
		readErr := tx.QueryRowContext(t.Context(), `select identity,label_ids from series where id=1`).Scan(&identity, &ids)
		if !strings.HasPrefix(identity, "@") {
			t.Fatalf("identity is not a digest: %q", identity)
		}
		decoded, decodeErr := decodeLabelIDs(ids)
		if decodeErr != nil || len(decoded) != 2 {
			t.Fatalf("stored identifiers %v: %v", decoded, decodeErr)
		}
		return readErr
	}); err != nil {
		t.Fatal(err)
	}
}

func TestLabelBudgetsRefuseOversizedNamesAndValues(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	long := strings.Repeat("x", maxLabelValueBytes+1)
	for _, labels := range [][]label{
		{{Name: "__name__", Value: "cpu"}, {Name: strings.Repeat("n", maxLabelNameBytes+1), Value: "v"}},
		{{Name: "__name__", Value: "cpu"}, {Name: "host", Value: long}},
	} {
		series := publicSeries(labels, Gauge)
		err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: testSamples(1)}})
		if !errors.Is(err, ErrInvalid) {
			t.Fatalf("oversized label accepted: %v", err)
		}
	}
}

func TestFormatLabelsPrintsTheMetricNameFirst(t *testing.T) {
	for _, test := range []struct {
		labels []label
		want   string
	}{
		{[]label{{"__name__", "cpu"}, {"host", "web-1"}, {"zone", "a"}}, `cpu{host="web-1",zone="a"}`},
		{[]label{{"__name__", "up"}}, `up`},
		{[]label{{"host", `a"b`}}, `{host="a\"b"}`},
	} {
		if got := formatLabels(test.labels); got != test.want {
			t.Errorf("%v: got %s, want %s", test.labels, got, test.want)
		}
	}
}

func TestSeriesErrorNamesOnlyRefusals(t *testing.T) {
	labels := []label{{Name: "__name__", Value: "cpu"}}
	var named *SeriesError
	if err := seriesError(labels, fmt.Errorf("%w: sealed frontier", ErrTooOld)); !errors.As(err, &named) {
		t.Fatalf("a refusal lost its series: %v", err)
	}
	for _, cause := range []error{context.Canceled, errors.New("disk I/O error")} {
		if err := seriesError(labels, cause); errors.As(err, &named) || !errors.Is(err, cause) {
			t.Fatalf("%v was blamed on a series: %v", cause, err)
		}
	}
}

// a series is its name and its labels: a range finds it by either or both,
// and a result gives them back apart, the store's own __name__ never among
// its labels; a name beginning with __ is the store's
func TestASeriesIsItsNameAndItsLabels(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	requests := Series{Name: "http_requests_total", Kind: Counter, Labels: Labels{"route": "/users"}}
	cpu := Series{Name: "cpu", Kind: Gauge, Labels: Labels{"route": "/users"}}
	if err := store.Ingest(t.Context(), []Batch{
		{Series: requests, Samples: testSamples(1)}, {Series: cpu, Samples: testSamples(1)},
	}); err != nil {
		t.Fatal(err)
	}

	for _, test := range []struct {
		request Range
		want    []string
	}{
		{Range{Name: "http_requests_total"}, []string{"http_requests_total"}},
		{Range{Match: Labels{"route": "/users"}}, []string{"cpu", "http_requests_total"}},
		{Range{Name: "cpu", Match: Labels{"route": "/users"}}, []string{"cpu"}},
	} {
		results, err := store.Read(t.Context(), test.request)
		var names []string
		for _, result := range results {
			names = append(names, result.Series.Name)
			if !maps.Equal(result.Series.Labels, Labels{"route": "/users"}) {
				t.Errorf("%+v: labels %v", test.request, result.Series.Labels)
			}
		}
		slices.Sort(names)
		if err != nil || !slices.Equal(names, test.want) {
			t.Errorf("%+v found %v, %v", test.request, names, err)
		}
	}

	for _, refused := range []Range{{}, {Match: Labels{"__name__": "cpu"}}} {
		if _, err := store.Read(t.Context(), refused); !errors.Is(err, ErrInvalid) {
			t.Errorf("%+v: %v", refused, err)
		}
	}
	unnamed := Series{Kind: Gauge, Labels: Labels{"host": "a"}}
	if err := store.Ingest(t.Context(), []Batch{{Series: unnamed, Samples: testSamples(1)}}); !errors.Is(err, ErrInvalid) {
		t.Errorf("a series without a name: %v", err)
	}
}
