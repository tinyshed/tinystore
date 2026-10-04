package server

import (
	"math"
	"reflect"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/server/internal/client"
	"github.com/tinyshed/tinystore/server/wire"
)

func ingest(t *testing.T, conn *client.Conn, series ...wire.MetricsSeries) error {
	t.Helper()
	_, err := conn.Call(t.Context(), wire.MetricsIngest, wire.MetricsBatch{Series: series})
	return err
}

// metricsDownload reads a read's or an aggregate's items, each decoded by read
func metricsDownload(t *testing.T, conn *client.Conn, method wire.Method, ask wire.MetricsRange,
	read func(body []byte) error,
) error {
	t.Helper()
	st, err := conn.Open(t.Context(), method, ask, true)
	if err != nil {
		return err
	}
	if _, err = st.Response(t.Context()); err != nil {
		return err
	}
	for {
		body, last, err := st.Next(t.Context())
		if err != nil || last {
			return err
		}
		if err = read(body); err != nil {
			return err
		}
	}
}

// readSeries reads a range, joining the pieces a series came in
func readSeries(t *testing.T, conn *client.Conn, ask wire.MetricsRange) ([]wire.MetricsSeries, int, error) {
	t.Helper()
	return downloadedSeries(t, conn, wire.MetricsRead, ask)
}

func downloadedSeries(
	t *testing.T, conn *client.Conn, method wire.Method, ask wire.MetricsRange,
) ([]wire.MetricsSeries, int, error) {
	t.Helper()
	var found []wire.MetricsSeries
	pieces := 0
	err := metricsDownload(t, conn, method, ask, func(body []byte) error {
		var piece wire.MetricsSeries
		if err := piece.Decode(body); err != nil {
			return err
		}
		pieces++
		if n := len(found); n > 0 && reflect.DeepEqual(found[n-1].Labels, piece.Labels) {
			found[n-1].Times = append(found[n-1].Times, piece.Times...)
			found[n-1].Values = append(found[n-1].Values, piece.Values...)
			return nil
		}
		found = append(found, piece)
		return nil
	})
	return found, pieces, err
}

func sameBits(a, b []float64) bool {
	return slices.EqualFunc(a, b, func(x, y float64) bool { return math.Float64bits(x) == math.Float64bits(y) })
}

// a sample comes back bit for bit, -0 and a NaN's payload included, with its
// series' labels and kind
func TestMetricsOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	if !slices.Contains(conn.Welcome.Engines, "metrics") {
		t.Fatalf("WELCOME serves %v", conn.Welcome.Engines)
	}
	now := time.Now().UnixMilli()
	cpu := wire.MetricsSeries{
		Labels: map[string]string{"__name__": "cpu", "host": "web-1"}, Kind: "gauge",
		Times:  []int64{now - 3000, now - 2000, now - 1000},
		Values: []float64{math.Copysign(0, -1), math.Float64frombits(0x7ff8000000000001), 0.1},
	}
	requests := wire.MetricsSeries{
		Labels: map[string]string{"__name__": "requests", "host": "web-1"},
		Kind:   "counter", Times: []int64{now - 1000}, Values: []float64{7},
	}
	if err := ingest(t, conn, cpu, requests); err != nil {
		t.Fatal(err)
	}

	found, _, err := readSeries(t, conn, wire.MetricsRange{
		Matchers: map[string]string{"__name__": "cpu"},
		From:     now - time.Hour.Milliseconds(), To: now + 1,
	})
	if err != nil || len(found) != 1 || !reflect.DeepEqual(found[0].Labels, cpu.Labels) || found[0].Kind != "gauge" ||
		!slices.Equal(found[0].Times, cpu.Times) || !sameBits(found[0].Values, cpu.Values) {
		t.Fatalf("cpu over the wire: %+v, %v", found, err)
	}
	found, _, err = readSeries(t, conn, wire.MetricsRange{
		Matchers: map[string]string{"host": "web-1"},
		From:     now - time.Hour.Milliseconds(), To: now + 1,
	})
	if err != nil || len(found) != 2 {
		t.Fatalf("both series of a host: %+v, %v", found, err)
	}
}

// an ingest stores every series or none, and a refused one is named by its
// labels
func TestAMetricsIngestIsAllOrNone(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	now := time.Now().UnixMilli()
	good := wire.MetricsSeries{
		Labels: map[string]string{"__name__": "cpu"}, Kind: "gauge", Times: []int64{now},
		Values: []float64{1},
	}
	for _, c := range []struct {
		name   string
		series wire.MetricsSeries
		code   wire.Code
	}{
		{"a kind nobody knows", wire.MetricsSeries{
			Labels: map[string]string{"__name__": "odd"}, Kind: "histogram",
			Times: []int64{now}, Values: []float64{1},
		}, wire.CodeInvalid},
		{"an hour ahead of the clock", wire.MetricsSeries{
			Labels: map[string]string{"__name__": "ahead"}, Kind: "gauge",
			Times: []int64{now + time.Hour.Milliseconds()}, Values: []float64{1},
		}, wire.CodeTooNew},
	} {
		failure := failureOf(ingest(t, conn, good, c.series))
		if failure.Code != c.code || !reflect.DeepEqual(failure.What, c.series.Labels) {
			t.Errorf("%s: %+v", c.name, failure)
		}
	}
	if _, err := conn.Call(t.Context(), wire.MetricsIngest, wire.MetricsBatch{Series: []wire.MetricsSeries{
		{Labels: map[string]string{"host": "nameless"}, Kind: "gauge", Times: []int64{now}, Values: []float64{1}},
	}}); failureOf(err).Code != wire.CodeInvalid {
		t.Errorf("a series without __name__: %v", err)
	}
	found, _, err := readSeries(t, conn, wire.MetricsRange{
		Matchers: map[string]string{"__name__": "cpu"},
		From:     now - 1, To: now + 1,
	})
	if err != nil || len(found) != 0 {
		t.Fatalf("a refused ingest stored %+v, %v", found, err)
	}
}

// A counter's increase counts each step, a reset's too, in the bucket it ends
// in, so buckets add up to the range; the first steps from the last sample a
// lookback before the range, one width unless the request names another.
func TestAggregateOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	start := time.Now().Add(-time.Minute).Truncate(time.Second).UnixMilli()
	requests := wire.MetricsSeries{
		Labels: map[string]string{"__name__": "requests"}, Kind: "counter",
		Times: []int64{start, start + 1000, start + 2000, start + 3000}, Values: []float64{100, 110, 5, 20},
	}
	if err := ingest(t, conn, requests); err != nil {
		t.Fatal(err)
	}
	ask := wire.MetricsRange{Matchers: requests.Labels, Op: "increase"}
	for _, c := range []struct {
		from, width, lookback int64
		want                  []wire.MetricsBucket
	}{
		{start, 4000, 0, []wire.MetricsBucket{{From: start, To: start + 4000, Count: 4, Resets: 1, Value: 30}}},
		{start, 2000, 0, []wire.MetricsBucket{
			{From: start, To: start + 2000, Count: 2, Value: 10},
			{From: start + 2000, To: start + 4000, Count: 2, Resets: 1, Value: 20},
		}},
		{start + 3000, 500, 0, []wire.MetricsBucket{{From: start + 3000, To: start + 3500, Count: 1}}},
		{start + 3000, 500, 1000, []wire.MetricsBucket{
			{From: start + 3000, To: start + 3500, Count: 1, Value: 15, Lookback: true},
		}},
	} {
		ask.From, ask.To, ask.Width, ask.Lookback = c.from, start+4000, c.width, c.lookback
		if buckets, err := aggregate(t, conn, ask); err != nil || !reflect.DeepEqual(buckets, c.want) {
			t.Errorf("increase from %d in buckets %d ms wide, a lookback of %d: %+v, %v",
				c.from-start, c.width, c.lookback, buckets, err)
		}
	}
	// an operation a newer client asks for is unimplemented, named, as an
	// unknown field is, rather than refused as a mistake
	ask.Op = "median"
	if _, err := aggregate(t, conn, ask); failureOf(err).Code != wire.CodeUnimplemented ||
		failureOf(err).What["op"] != "median" {
		t.Errorf("an operation this server does not have: %v", err)
	}
}

// A read's conditions travel beside its matchers, and a condition of a kind
// this server does not have is unimplemented, named, never skipped: a read
// without it would answer another question.
func TestConditionsOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	now := time.Now().UnixMilli()
	for _, status := range []string{"200", "500", "502"} {
		series := wire.MetricsSeries{
			Labels: map[string]string{"__name__": "requests", "status": status}, Kind: "counter",
			Times: []int64{now - 1000}, Values: []float64{1},
		}
		if err := ingest(t, conn, series); err != nil {
			t.Fatal(err)
		}
	}
	ask := wire.MetricsRange{
		Matchers: map[string]string{"__name__": "requests"}, From: now - time.Hour.Milliseconds(), To: now + 1,
		Where: []wire.MetricsCondition{{Label: "status", Kind: "one_of", Values: []string{"500", "502"}}},
	}
	found, _, err := readSeries(t, conn, ask)
	if err != nil || len(found) != 2 {
		t.Fatalf("one_of 500 and 502: %d series, %v", len(found), err)
	}

	ask.Where[0].Kind = "regex"
	if _, _, err = readSeries(t, conn, ask); failureOf(err).Code != wire.CodeUnimplemented ||
		failureOf(err).What["condition"] != "regex" {
		t.Fatalf("a condition this server does not have: %v", err)
	}
}

func aggregate(t *testing.T, conn *client.Conn, ask wire.MetricsRange) ([]wire.MetricsBucket, error) {
	t.Helper()
	var buckets []wire.MetricsBucket
	err := metricsDownload(t, conn, wire.MetricsAggregate, ask, func(body []byte) error {
		var piece wire.MetricsBuckets
		err := piece.Decode(body)
		buckets = append(buckets, piece.Buckets...)
		return err
	})
	return buckets, err
}

// a series longer than a body holds comes in pieces, each with its labels,
// which join into what went in
func TestALongSeriesComesInPieces(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{MaxBody: 4 << 10})
	start := time.Now().Add(-time.Hour).UnixMilli()
	series := wire.MetricsSeries{Labels: map[string]string{"__name__": "temperature"}, Kind: "gauge"}
	for i := range 1000 {
		series.Times = append(series.Times, start+int64(i))
		series.Values = append(series.Values, float64(i)/7)
	}
	for from := 0; from < len(series.Times); from += 200 {
		piece := series
		piece.Times, piece.Values = series.Times[from:from+200], series.Values[from:from+200]
		if err := ingest(t, conn, piece); err != nil {
			t.Fatal(err)
		}
	}
	found, pieces, err := readSeries(t, conn, wire.MetricsRange{Matchers: series.Labels, From: start, To: start + 1000})
	if err != nil || len(found) != 1 || !slices.Equal(found[0].Times, series.Times) ||
		!sameBits(found[0].Values, series.Values) || pieces < 8 {
		t.Fatalf("a long series in %d pieces: %d series, %v", pieces, len(found), err)
	}
}

// a dropped series is gone, and a second drop finds nothing
func TestDropSeriesOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	now := time.Now().UnixMilli()
	cpu := wire.MetricsSeries{
		Labels: map[string]string{"__name__": "cpu"}, Kind: "gauge", Times: []int64{now},
		Values: []float64{1},
	}
	if err := ingest(t, conn, cpu); err != nil {
		t.Fatal(err)
	}
	for _, want := range []bool{true, false} {
		body, err := conn.Call(t.Context(), wire.MetricsDrop, wire.MetricsLabels{Labels: cpu.Labels})
		var dropped wire.MetricsDropped
		if err != nil || dropped.Decode(body) != nil || dropped.Found != want {
			t.Fatalf("a drop: %+v, %v, want found %v", dropped, err, want)
		}
	}
	if found, _, err := readSeries(t, conn, wire.MetricsRange{Matchers: cpu.Labels, From: now - 1, To: now + 1}); err !=
		nil || len(found) != 0 {
		t.Fatalf("a dropped series read as %+v, %v", found, err)
	}
}

// Latest answers each series' newest sample in the range, one a DATA, and
// leaves out a series that stopped before it.
func TestLatestOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	now := time.Now().UnixMilli()
	for host, times := range map[string][]int64{"web": {now - 2000, now - 1000}, "db": {now - 600_000}} {
		series := wire.MetricsSeries{
			Labels: map[string]string{"__name__": "cpu", "host": host}, Kind: "gauge",
			Times: times, Values: make([]float64, len(times)),
		}
		series.Values[len(times)-1] = 0.5
		if err := ingest(t, conn, series); err != nil {
			t.Fatal(err)
		}
	}
	ask := wire.MetricsRange{Matchers: map[string]string{"__name__": "cpu"}, From: now - 60_000, To: now + 1}
	found, pieces, err := downloadedSeries(t, conn, wire.MetricsLatest, ask)
	if err != nil || pieces != 1 || len(found) != 1 || found[0].Labels["host"] != "web" ||
		!reflect.DeepEqual(found[0].Times, []int64{now - 1000}) || !reflect.DeepEqual(found[0].Values, []float64{0.5}) {
		t.Fatalf("the latest of a minute: %+v, %d pieces: %v", found, pieces, err)
	}
}

// A description travels both ways: describe keeps it, described answers it,
// and a name never described answers none.
func TestADescriptionOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	kept := wire.MetricsDescription{Name: "query_sum", Unit: "ms", Help: "How long database queries took."}
	if _, err := conn.Call(t.Context(), wire.MetricsDescribe, kept); err != nil {
		t.Fatal(err)
	}
	for name, want := range map[string]wire.MetricsDescription{"query_sum": kept, "other": {Name: "other"}} {
		body, err := conn.Call(t.Context(), wire.MetricsDescribed, wire.MetricsDescription{Name: name})
		var got wire.MetricsDescription
		if err == nil {
			err = got.Decode(body)
		}
		if err != nil || got != want {
			t.Fatalf("described %s: %+v, %v", name, got, err)
		}
	}
	long := wire.MetricsDescription{Name: "query_sum", Unit: strings.Repeat("m", 33)}
	if _, err := conn.Call(t.Context(), wire.MetricsDescribe, long); failureOf(err).Code != wire.CodeInvalid {
		t.Fatalf("a unit past its bound: %v", err)
	}
}
