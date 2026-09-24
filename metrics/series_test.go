package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"strings"
	"testing"
)

func TestDigestNeverAliasesDifferentLabels(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	first := testSeries()
	second := testSeries()
	second.Labels[0].Value = "another"
	if err := store.Ingest(t.Context(), []Batch{{Series: first, Samples: testSamples(1)}}); err != nil {
		t.Fatal(err)
	}
	_, canonical, err := canonicalLabels(second.Labels, true)
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
	second.Labels[0].Value = "two"
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
		result, err := store.Read(t.Context(), Range{Matchers: series.Labels, From: testEpoch, To: testEpoch + 1})
		if err != nil || len(result) != 1 {
			t.Fatal("dictionary matching", err, result)
		}
	}
}

func TestContainerSeriesKeepsEveryLabelThroughTheDictionary(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	series := Series{Labels: []Label{{Name: "__name__", Value: "docker_container_mem_usage"}}, Kind: Gauge}
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
		series.Labels = append(series.Labels, Label{Name: name, Value: name + "-value"})
	}
	if len(series.Labels) != 38 {
		t.Fatalf("fixture carries %d labels", len(series.Labels))
	}
	points := testSamples(4)
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	result, err := store.Read(t.Context(), Range{Matchers: series.Labels, From: testEpoch, To: testEpoch + int64(len(points))})
	if err != nil || len(result) != 1 {
		t.Fatal("container series is unreadable", err, len(result))
	}
	want, _, err := canonicalLabels(series.Labels, true)
	if err != nil {
		t.Fatal(err)
	}
	if len(result[0].Series.Labels) != len(want) {
		t.Fatalf("got %d labels back, wrote %d", len(result[0].Series.Labels), len(want))
	}
	for i, label := range result[0].Series.Labels {
		if label != want[i] {
			t.Fatalf("label %d came back as %v, not %v", i, label, want[i])
		}
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
	for _, labels := range [][]Label{
		{{Name: "__name__", Value: "cpu"}, {Name: strings.Repeat("n", maxLabelNameBytes+1), Value: "v"}},
		{{Name: "__name__", Value: "cpu"}, {Name: "host", Value: long}},
	} {
		err := store.Ingest(t.Context(), []Batch{{Series: Series{Labels: labels, Kind: Gauge}, Samples: testSamples(1)}})
		if !errors.Is(err, ErrInvalid) {
			t.Fatalf("oversized label accepted: %v", err)
		}
	}
}

func TestFormatLabelsPrintsTheMetricNameFirst(t *testing.T) {
	for _, test := range []struct {
		labels []Label
		want   string
	}{
		{[]Label{{"__name__", "cpu"}, {"host", "web-1"}, {"zone", "a"}}, `cpu{host="web-1",zone="a"}`},
		{[]Label{{"__name__", "up"}}, `up`},
		{[]Label{{"host", `a"b`}}, `{host="a\"b"}`},
	} {
		if got := formatLabels(test.labels); got != test.want {
			t.Errorf("%v: got %s, want %s", test.labels, got, test.want)
		}
	}
}

func TestSeriesErrorNamesOnlyRefusals(t *testing.T) {
	labels := []Label{{Name: "__name__", Value: "cpu"}}
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
