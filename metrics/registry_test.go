package metrics

import (
	"database/sql"
	"encoding/json"
	"errors"
	"strings"
	"testing"
)

func TestLegacyLabelIdentityIsReusedAndCompacted(t *testing.T) {
	store, _ := openTestStore(t, Options{})
	series := testSeries()
	points := testSamples(10)
	if err := store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	labels, _, err := canonicalLabels(series.Labels, true)
	if err != nil {
		t.Fatal(err)
	}
	legacy, err := json.Marshal(labels)
	if err != nil {
		t.Fatal(err)
	}
	if err = store.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, writeErr := tx.ExecContext(t.Context(), `update series set identity=?,labels=null where id=1`, string(legacy))
		return writeErr
	}); err != nil {
		t.Fatal(err)
	}
	assertSamples(t, readAll(t, store), points)
	if err = store.Ingest(t.Context(), []Batch{{Series: series, Samples: points}}); err != nil {
		t.Fatal(err)
	}
	if err = store.file.View(t.Context(), func(tx *sql.Tx) error {
		var count, keyBytes int
		readErr := tx.QueryRowContext(t.Context(), `select count(*),length(identity) from series`).Scan(&count, &keyBytes)
		if count != 1 || keyBytes != 44 {
			t.Fatalf("identity migration count=%d bytes=%d", count, keyBytes)
		}
		return readErr
	}); err != nil {
		t.Fatal(err)
	}
}

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
		var text sql.NullString
		var ids []byte
		readErr := tx.QueryRowContext(t.Context(), `select labels,label_ids from series where id=1`).Scan(&text, &ids)
		if text.Valid {
			t.Fatalf("label text survived: %q", text.String)
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
