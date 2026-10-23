package metrics_test

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/ways"
	"tinyshed.org/tinystore/kv"
	"tinyshed.org/tinystore/metrics"
)

func is(t *testing.T, err error, kind error) {
	t.Helper()
	if !errors.Is(err, kind) {
		t.Fatalf("got %v, want %v", err, kind)
	}
}

// last is the last value of each series of a name, by its labels.
func last(t *testing.T, store *tinystore.Store, name string) map[string]float64 {
	t.Helper()
	read := ways.Must(metrics.Read(context.Background(), store, metrics.Query{Name: name, Since: time.Hour}))
	out := map[string]float64{}
	for _, series := range read {
		if len(series.Samples) > 0 {
			labels, _ := json.Marshal(series.Labels)
			out[string(labels)] = series.Samples[len(series.Samples)-1].Value
		}
	}
	return out
}

func TestASeriesComesBackBitForBit(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		at := time.Now().Add(-time.Minute).Truncate(time.Millisecond).UTC()
		payload := math.Float64frombits(0x7ff8_0000_dead_beef)
		values := []float64{0.42, math.Copysign(0, -1), payload, math.Inf(1)}
		times := []time.Time{at, at.Add(time.Second), at.Add(2 * time.Second), at.Add(3 * time.Second)}
		ways.Must(0, metrics.Ingest(ctx, store, metrics.Series{Name: "cpu", Kind: metrics.KindGauge, Labels: metrics.Labels{"host": "web-1"}, Times: times, Values: values}))
		read := ways.Must(metrics.Read(ctx, store, metrics.Query{Name: "cpu", Where: metrics.Where{"host": "web-1"}, Since: time.Hour}))
		if len(read) != 1 || read[0].Labels["host"] != "web-1" || len(read[0].Samples) != 4 {
			t.Fatalf("read %+v", read)
		}
		for i, sample := range read[0].Samples {
			if !sample.At.Equal(times[i]) || math.Float64bits(sample.Value) != math.Float64bits(values[i]) {
				t.Fatalf("sample %d: %v %x, want %v %x", i, sample.At, math.Float64bits(sample.Value), times[i], math.Float64bits(values[i]))
			}
		}
		newest := ways.Must(metrics.Latest(ctx, store, metrics.Query{Name: "cpu", Since: time.Hour}))
		if len(newest) != 1 || !newest[0].At.Equal(times[3]) || newest[0].Labels["host"] != "web-1" {
			t.Fatalf("latest %+v", newest)
		}
	})
}

// prometheusVectors is testdata/metrics/prometheus.json: texts an exporter
// serves and the series each writes, which every language reads alike.
type prometheusVectors struct {
	At    int64  `json:"at"`
	Keep  string `json:"keep"`
	Cases []struct {
		Name   string            `json:"name"`
		Text   string            `json:"text"`
		Labels map[string]string `json:"labels"`
		Series []struct {
			Name    string            `json:"name"`
			Kind    string            `json:"kind"`
			Labels  map[string]string `json:"labels"`
			Samples [][2]any          `json:"samples"`
		} `json:"series"`
		Error *struct {
			Kind string `json:"kind"`
			Has  string `json:"has"`
		} `json:"error"`
	} `json:"cases"`
}

// seenSeries is a series as a vector compares it: its samples' times and their values' bits.
type seenSeries struct {
	name, kind, labels string
	samples            string
}

func seenOf(name string, kind metrics.Kind, labels map[string]string, samples [][2]int64) seenSeries {
	if labels == nil {
		labels = map[string]string{}
	}
	spelled, _ := json.Marshal(labels)
	written, _ := json.Marshal(samples)
	return seenSeries{name, string(kind), string(spelled), string(written)}
}

// contract: MET-15
func TestEveryTextOfTheVectorsWritesItsSeriesOrIsRefusedWhole(t *testing.T) {
	text, err := os.ReadFile(filepath.Join("..", "..", "..", "testdata", "metrics", "prometheus.json"))
	if err != nil {
		t.Fatal(err)
	}
	var vectors prometheusVectors
	ways.Must(0, json.Unmarshal(text, &vectors))
	if vectors.Keep != "3650d" {
		t.Fatalf("keep %q", vectors.Keep)
	}
	keep, at := 3650*24*time.Hour, time.UnixMilli(vectors.At)
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		for _, each := range vectors.Cases {
			err := metrics.IngestPrometheus(ctx, store, []byte(each.Text), metrics.Labels(each.Labels), metrics.At(at), metrics.Keep(keep))
			if each.Error != nil {
				var refused *tinystore.Error
				if !errors.As(err, &refused) || string(refused.Kind) != each.Error.Kind || !strings.Contains(refused.Message, each.Error.Has) {
					t.Fatalf("%s: %v, want %s %q", each.Name, err, each.Error.Kind, each.Error.Has)
				}
				continue
			}
			if err != nil {
				t.Fatalf("%s: %v", each.Name, err)
			}
			var want, got []seenSeries
			names := map[string]bool{}
			for _, series := range each.Series {
				samples := make([][2]int64, len(series.Samples))
				for i, sample := range series.Samples {
					// Go's NaN carries a payload of 1; the text's NaN is the quiet one every other language parses
					bits := uint64(0x7ff8_0000_0000_0000)
					if sample[1] != "NaN" {
						bits = math.Float64bits(ways.Must(strconv.ParseFloat(sample[1].(string), 64)))
					}
					samples[i] = [2]int64{int64(sample[0].(float64)), int64(bits)}
				}
				want = append(want, seenOf(series.Name, metrics.Kind(series.Kind), series.Labels, samples))
				names[series.Name] = true
			}
			for name := range names {
				for _, series := range ways.Must(metrics.Read(ctx, store, metrics.Query{Name: name, Since: keep})) {
					samples := make([][2]int64, len(series.Samples))
					for i, sample := range series.Samples {
						samples[i] = [2]int64{sample.At.UnixMilli(), int64(math.Float64bits(sample.Value))}
					}
					got = append(got, seenOf(series.Name, series.Kind, series.Labels, samples))
				}
			}
			bySeries := func(list []seenSeries) func(i, j int) bool {
				return func(i, j int) bool { return fmt.Sprint(list[i]) < fmt.Sprint(list[j]) }
			}
			sort.Slice(want, bySeries(want))
			sort.Slice(got, bySeries(got))
			if !reflect.DeepEqual(got, want) {
				t.Fatalf("%s:\n got %v\nwant %v", each.Name, got, want)
			}
		}
		is(t, metrics.IngestPrometheus(ctx, store, []byte("up 1\n"), metrics.Help("no")), tinystore.ErrInvalid)
	})
}

func TestAnAggregatesBucketsAreExactByGroupAndATimersPercentileIsItsBounds(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		at := time.Now().Add(-time.Minute)
		route := func(name string, values ...float64) metrics.Series {
			samples := make([]metrics.Sample, len(values))
			for i, value := range values {
				samples[i] = metrics.Sample{At: at.Add(time.Duration(i) * time.Second), Value: value}
			}
			return metrics.Series{Name: "requests_total", Kind: metrics.KindCounter, Labels: metrics.Labels{"route": name}, Samples: samples}
		}
		ways.Must(0, metrics.Ingest(ctx, store, route("/a", 0, 10, 25), route("/b", 0, 3, 1)))
		traffic := ways.Must(metrics.Aggregate(ctx, store, metrics.Query{Name: "requests_total", Since: time.Hour, Op: metrics.Increase, By: []string{"route"}}))
		byRoute := map[string]metrics.Bucket{}
		for _, group := range traffic {
			byRoute[group.Labels["route"]] = group.Buckets[0]
		}
		if a := byRoute["/a"]; *a.Value != 25 || a.Resets != 0 {
			t.Fatalf("/a %v %d", *a.Value, a.Resets)
		}
		if b := byRoute["/b"]; *b.Value != 4 || b.Resets != 1 {
			t.Fatalf("/b %v %d", *b.Value, b.Resets)
		}
		routes := func(where metrics.Where) []string {
			read := ways.Must(metrics.Read(ctx, store, metrics.Query{Name: "requests_total", Where: where, Since: time.Hour}))
			var out []string
			for _, series := range read {
				out = append(out, series.Labels["route"])
			}
			return out
		}
		if got := routes(metrics.Where{"route": metrics.Not("/a")}); !reflect.DeepEqual(got, []string{"/b"}) {
			t.Fatalf("not /a %v", got)
		}
		if got := routes(metrics.Where{"route": metrics.OneOf("/a", "/b")}); len(got) != 2 {
			t.Fatalf("one of %v", got)
		}
		if got := routes(metrics.Where{"route": metrics.Prefix("/")}); len(got) != 2 {
			t.Fatalf("prefix %v", got)
		}
		latency := ways.Must(metrics.OpenTimer(store, "db_ms", metrics.Buckets(5, 10, 25)))
		for _, took := range []time.Duration{3, 7, 20, 100} {
			latency.Record(took * time.Millisecond)
		}
		ways.Must(0, metrics.Flush(ctx, store))
		p50 := ways.Must(metrics.Aggregate(ctx, store, metrics.Query{Name: "db_ms", Since: time.Hour, Op: metrics.Percentile(50)}))
		if b := p50[0].Buckets[0]; *b.Lower != 5 || *b.Upper != 10 || b.Samples != 4 {
			t.Fatalf("p50 %v %v %d", *b.Lower, *b.Upper, b.Samples)
		}
		p999 := ways.Must(metrics.Aggregate(ctx, store, metrics.Query{Name: "db_ms", Since: time.Hour, Op: metrics.Percentile(99.9), Estimate: true}))
		if b := p999[0].Buckets[0]; *b.Lower != 25 || !math.IsInf(*b.Upper, 1) || *b.Estimate != 25 {
			t.Fatalf("p99.9 %v %v %v", *b.Lower, *b.Upper, *b.Estimate)
		}
		mean := ways.Must(metrics.Aggregate(ctx, store, metrics.Query{Name: "db_ms", Since: time.Hour, Op: metrics.Avg}))
		if *mean[0].Buckets[0].Value != 32.5 {
			t.Fatalf("avg %v", *mean[0].Buckets[0].Value)
		}
		_, err := metrics.Aggregate(ctx, store, metrics.Query{Name: "db_ms", Since: time.Hour, Op: "p"})
		is(t, err, tinystore.ErrInvalid)
	})
}

func TestInstrumentsCountIntoTheStoresTotalsASetOfLabelsASeriesEach(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		requests := ways.Must(metrics.OpenCounter(store, "http_requests_total", metrics.Help("Requests served.")))
		requests.With("route", "/notes", "status", 200).Inc()
		requests.With("status", "200", "route", "/notes").Add(2)
		requests.With("route", "/users", "status", 200).Add(0)
		depth := ways.Must(metrics.OpenGauge(store, "queue_depth", metrics.Unit("jobs")))
		depth.Set(12)
		depth.Inc()
		ways.Must(0, metrics.OpenGaugeFunc(store, "open_tickets", func(context.Context) (float64, error) { return 7, nil }))
		sizes := ways.Must(metrics.OpenHistogram(store, "response_bytes", metrics.Unit("bytes"), metrics.Buckets(100, 1000)))
		sizes.Record(50)
		sizes.Record(5000)
		ways.Must(0, metrics.Flush(ctx, store))
		want := map[string]float64{`{"route":"/notes","status":"200"}`: 3, `{"route":"/users","status":"200"}`: 0}
		if got := last(t, store, "http_requests_total"); !reflect.DeepEqual(got, want) {
			t.Fatalf("requests %v", got)
		}
		for name, want := range map[string]float64{"queue_depth": 13, "open_tickets": 7, "response_bytes_count": 2, "response_bytes_sum": 5050} {
			if got := last(t, store, name); got["null"]+got["{}"] != want {
				t.Fatalf("%s %v", name, got)
			}
		}
		if described := ways.Must(metrics.DescriptionOf(ctx, store, "http_requests_total")); described != (metrics.Description{Help: "Requests served."}) {
			t.Fatalf("description %+v", described)
		}
	})
}

func TestATimersTimingRecordsOnceWhenItStops(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		took := ways.Must(metrics.OpenTimer(store, "measured_ms"))
		func() { defer took.Start().Stop() }()
		timing := took.Start()
		if timing.Stop() < 0 {
			t.Fatal("a timing took less than nothing")
		}
		timing.Stop()
		took.Record(250 * time.Millisecond)
		ways.Must(0, metrics.Flush(ctx, store))
		if got := last(t, store, "measured_ms_count"); got["null"]+got["{}"] != 3 {
			t.Fatalf("count %v", got)
		}
	})
}

func TestAnInstrumentTheStoreRefusesFailsTheNextFlushAndKeepsNoOtherOut(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		ways.Must(0, metrics.Ingest(ctx, store, metrics.Series{Name: "taken", Kind: metrics.KindGauge, Samples: []metrics.Sample{{At: time.Now().Add(-time.Second), Value: 1}}}))
		ways.Must(metrics.OpenCounter(store, "taken")).Inc()
		ways.Must(metrics.OpenCounter(store, "others_total")).Inc()
		is(t, metrics.Flush(ctx, store), tinystore.ErrInvalid)
		ways.Must(0, metrics.Flush(ctx, store))
		if got := last(t, store, "others_total"); got["null"]+got["{}"] != 1 {
			t.Fatalf("others %v", got)
		}
		if got := last(t, store, "taken"); got["null"]+got["{}"] != 1 {
			t.Fatalf("taken %v", got)
		}
	})
}

func TestACallThatWouldTakeASumPastTheLargestNumberChangesNothing(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		huge := ways.Must(metrics.OpenHistogram(store, "huge_bytes"))
		huge.Record(1e308)
		huge.Record(1e308)
		added := ways.Must(metrics.OpenCounter(store, "huge_total"))
		added.Add(1e308)
		added.Add(1e308)
		ways.Must(0, metrics.Flush(ctx, store))
		if got := last(t, store, "huge_bytes_count"); got["null"]+got["{}"] != 1 {
			t.Fatalf("count %v", got)
		}
		if got := last(t, store, "huge_total"); got["null"]+got["{}"] != 1e308 {
			t.Fatalf("total %v", got)
		}
	})
}

func TestAGaugesFunctionsReadASetOfLabelsEachAndASetIsReadByOne(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		returning := func(value float64) func(context.Context) (float64, error) {
			return func(context.Context) (float64, error) { return value, nil }
		}
		ways.Must(0, metrics.OpenGaugeFunc(store, "queue_depth_jobs", returning(3), metrics.Labels{"queue": "emails"}))
		ways.Must(0, metrics.OpenGaugeFunc(store, "queue_depth_jobs", returning(5), metrics.Labels{"queue": "sms"}))
		is(t, metrics.OpenGaugeFunc(store, "queue_depth_jobs", returning(7), metrics.Labels{"queue": "sms"}), tinystore.ErrInvalid)
		_, err := metrics.OpenGauge(store, "queue_depth_jobs")
		is(t, err, tinystore.ErrInvalid)
		ways.Must(0, metrics.OpenGaugeFunc(store, "failing_depth", func(context.Context) (float64, error) { return 0, errors.New("down") }))
		ways.Must(0, metrics.Flush(ctx, store))
		want := map[string]float64{`{"queue":"emails"}`: 3, `{"queue":"sms"}`: 5}
		if got := last(t, store, "queue_depth_jobs"); !reflect.DeepEqual(got, want) {
			t.Fatalf("depth %v", got)
		}
		if got := last(t, store, "failing_depth"); len(got) != 0 {
			t.Fatalf("a failing function wrote %v", got)
		}
	})
}

func TestACallAnInstrumentRefusesChangesNothingAndAnOpenItRefusesFails(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		counter := ways.Must(metrics.OpenCounter(store, "refusals_total"))
		counter.Add(-1)
		counter.Add(math.NaN())
		counter.With("__store", "a").Inc()
		counter.With("odd").Inc()
		ways.Must(metrics.OpenGauge(store, "refused_depth")).Set(math.Inf(1))
		timer := ways.Must(metrics.OpenTimer(store, "refused_ms"))
		timer.Record(-5 * time.Millisecond)
		timer.With("le", "5").Record(time.Millisecond)
		ways.Must(0, metrics.Flush(ctx, store))
		for _, name := range []string{"refusals_total", "refused_depth", "refused_ms_count"} {
			if got := last(t, store, name); len(got) != 0 {
				t.Fatalf("%s: a refused call wrote %v", name, got)
			}
		}
		_, err := metrics.OpenTimer(store, "unordered_ms", metrics.Buckets(10, 5))
		is(t, err, tinystore.ErrInvalid)
		_, err = metrics.OpenGauge(store, "refusals_total")
		is(t, err, tinystore.ErrInvalid)
		_, err = metrics.OpenCounter(store, "refusals_total", metrics.Help("another"))
		is(t, err, tinystore.ErrInvalid)
		_, err = metrics.OpenCounter(store, "bucketed_total", metrics.Buckets(1))
		is(t, err, tinystore.ErrInvalid)
		one := func(context.Context) (float64, error) { return 1, nil }
		ways.Must(0, metrics.OpenGaugeFunc(store, "read_once", one))
		is(t, metrics.OpenGaugeFunc(store, "read_once", one), tinystore.ErrInvalid)
	})
}

func TestACallMadeWhileABatchIsOnItsWayCountsInTheNext(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		jobs := ways.Must(metrics.OpenCounter(store, "jobs_total"))
		for range 20 {
			jobs.Inc()
			var flushing sync.WaitGroup
			flushing.Go(func() { ways.Must(0, metrics.Flush(ctx, store)) })
			jobs.Inc()
			flushing.Wait()
		}
		ways.Must(0, metrics.Flush(ctx, store))
		if got := last(t, store, "jobs_total"); got["null"]+got["{}"] != 40 {
			t.Fatalf("jobs %v", got)
		}
	})
}

func TestWhatAStoreHoldsAPageAtATimeAndItsRemoval(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		requests := ways.Must(metrics.OpenCounter(store, "http_requests_total", metrics.Help("Requests served.")))
		requests.With("route", "/notes", "status", 200).Inc()
		requests.With("route", "/users", "status", 200).Inc()
		ways.Must(0, metrics.Flush(ctx, store))
		ways.Must(0, metrics.Ingest(ctx, store, metrics.Series{Name: "cpu", Kind: metrics.KindGauge, Labels: metrics.Labels{"host": "web-1"},
			Samples: []metrics.Sample{{At: time.Now().Add(-time.Second), Value: 0.5}}}))
		page := ways.Must(metrics.Names(ctx, store, metrics.Prefix("http_")))
		if len(page.Names) != 1 || page.Next != "" {
			t.Fatalf("page %+v", page)
		}
		held := page.Names[0]
		if held.Name != "http_requests_total" || held.Kind != metrics.KindCounter || held.SeriesCount != 2 || held.Help != "Requests served." ||
			held.Keep != 30*24*time.Hour || held.RetainedFrom.After(held.Newest) {
			t.Fatalf("held %+v", held)
		}
		first := ways.Must(metrics.Names(ctx, store, metrics.PageLimit(1)))
		rest := ways.Must(metrics.Names(ctx, store, metrics.After(first.Next)))
		if len(first.Names) != 1 || len(rest.Names) != 1 || rest.Names[0].Name == first.Names[0].Name {
			t.Fatalf("first %+v rest %+v", first, rest)
		}
		if labels := ways.Must(metrics.LabelNames(ctx, store, "http_requests_total")); !reflect.DeepEqual(labels.Texts, []string{"route", "status"}) {
			t.Fatalf("labels %+v", labels)
		}
		if values := ways.Must(metrics.LabelValues(ctx, store, "http_requests_total", "route", metrics.Prefix("/n"))); !reflect.DeepEqual(values.Texts, []string{"/notes"}) {
			t.Fatalf("values %+v", values)
		}
		ways.Must(0, metrics.Describe(ctx, store, "cpu", metrics.Description{Unit: "%", Help: "Busy."}))
		if described := ways.Must(metrics.DescriptionOf(ctx, store, "cpu")); described != (metrics.Description{Unit: "%", Help: "Busy."}) {
			t.Fatalf("description %+v", described)
		}
		ways.Must(0, metrics.Describe(ctx, store, "cpu", metrics.Description{}))
		if described := ways.Must(metrics.DescriptionOf(ctx, store, "cpu")); described != (metrics.Description{}) {
			t.Fatalf("description %+v", described)
		}
		if !ways.Must(metrics.Delete(ctx, store, "cpu", metrics.Labels{"host": "web-1"})) || ways.Must(metrics.Delete(ctx, store, "cpu", metrics.Labels{"host": "web-1"})) {
			t.Fatal("delete says whether the store held the series")
		}
		if damaged := ways.Must(metrics.Damaged(ctx, store)); len(damaged) != 0 {
			t.Fatalf("damaged %+v", damaged)
		}
		plan := ways.Must(metrics.Explain(ctx, store, metrics.Query{Name: "http_requests_total", Since: time.Hour, Every: time.Minute, Op: metrics.Rate}))
		if plan.Series != 2 || plan.Exceeds != nil {
			t.Fatalf("plan %+v", plan)
		}
	})
}

func TestAQueryIsRefusedBeforeItLeaves(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		_, err := metrics.Read(ctx, store, metrics.Query{Name: "cpu", Since: time.Hour, From: time.Now()})
		is(t, err, tinystore.ErrInvalid)
		_, err = metrics.Read(ctx, store, metrics.Query{Name: "cpu", Where: metrics.Where{"host": nil}})
		is(t, err, tinystore.ErrInvalid)
		err = metrics.Ingest(ctx, store, metrics.Series{Name: "cpu", Kind: metrics.KindGauge, Times: []time.Time{time.Now(), time.Now()}, Values: []float64{1}})
		is(t, err, tinystore.ErrInvalid)
	})
}

func TestAStoreClosedWithItsInstrumentsHandsOverWhatTheyHoldAndLetsGoAtOnce(t *testing.T) {
	ways.EachWay(t, func(t *testing.T, way ways.Way) {
		dir := t.TempDir()
		store := way.Open(t, dir)
		ways.Must(metrics.OpenCounter(store, "closing_total")).Add(5)
		started := time.Now()
		ways.Must(0, store.Close())
		if took := time.Since(started); took > 3*time.Second {
			t.Fatalf("the close took %s", took)
		}
		if !way.Shared {
			return
		}
		again := way.Open(t, dir)
		defer again.Close()
		if got := last(t, again, "closing_total"); got["null"]+got["{}"] != 5 {
			t.Fatalf("the close kept %v", got)
		}
	})
}

func TestAStoreOpenedWithSelfMetricsMeasuresItsCommits(t *testing.T) {
	ctx := context.Background()
	library, binary := os.Getenv("TINYSTORE_LIBRARY"), os.Getenv("TINYSTORE_BIN")
	measured := []struct {
		name  string
		ready bool
		opts  []tinystore.Option
	}{
		{"the core in this process", library != "", []tinystore.Option{tinystore.Library(library)}},
		{"a private child", binary != "" && binary != "unused", []tinystore.Option{tinystore.Private(), tinystore.Binary(binary)}},
	}
	for _, way := range measured {
		t.Run(way.name, func(t *testing.T) {
			if !way.ready {
				t.Skip("not built here")
			}
			defer ways.Recover(t)
			store := ways.Must(tinystore.Open(ctx, t.TempDir(), append(way.opts, tinystore.SelfMetrics())...))
			defer store.Close()
			sessions := ways.Must(kv.OpenBucket[string](store, "sessions"))
			ways.Must(struct{}{}, sessions.Set(ctx, "a", "x"))
			ways.Must(struct{}{}, metrics.Flush(ctx, store))
			query := metrics.Query{Name: "tinystore_commits_total", Where: metrics.Where{"file": "kv"}, Since: time.Hour}
			newest := ways.Must(metrics.Latest(ctx, store, query))
			if len(newest) != 1 || newest[0].Value <= 0 {
				t.Fatalf("tinystore_commits_total{file=kv}: %+v", newest)
			}
		})
	}
}

func TestAStoreHoldsTheSeriesMaxSeriesSays(t *testing.T) {
	ctx := context.Background()
	library, binary := os.Getenv("TINYSTORE_LIBRARY"), os.Getenv("TINYSTORE_BIN")
	bounded := []struct {
		name  string
		ready bool
		opts  []tinystore.Option
	}{
		{"the core in this process", library != "", []tinystore.Option{tinystore.Library(library)}},
		{"a private child", binary != "" && binary != "unused", []tinystore.Option{tinystore.Private(), tinystore.Binary(binary)}},
	}
	host := func(name string) metrics.Series {
		samples := []metrics.Sample{{At: time.Now().Add(-time.Second), Value: 1}}
		return metrics.Series{Name: "cpu", Kind: metrics.KindGauge, Labels: metrics.Labels{"host": name}, Samples: samples}
	}
	for _, way := range bounded {
		t.Run(way.name, func(t *testing.T) {
			if !way.ready {
				t.Skip("not built here")
			}
			defer ways.Recover(t)
			store := ways.Must(tinystore.Open(ctx, t.TempDir(), append(way.opts, tinystore.MaxSeries(2))...))
			defer store.Close()
			ways.Must(0, metrics.Ingest(ctx, store, host("a"), host("b")))
			is(t, metrics.Ingest(ctx, store, host("c")), tinystore.ErrLimit)
		})
	}
	_, err := tinystore.Open(ctx, t.TempDir(), tinystore.MaxSeries(0))
	is(t, err, tinystore.ErrInvalid)
}

func TestSelfMetricsMeasureTheMachineUnlessWithoutSystem(t *testing.T) {
	ctx := context.Background()
	library := os.Getenv("TINYSTORE_LIBRARY")
	if library == "" {
		t.Skip("the core in this process: not built here")
	}
	defer ways.Recover(t)
	for _, way := range []struct {
		opt      tinystore.Option
		measured bool
	}{{tinystore.SelfMetrics(), true}, {tinystore.SelfMetrics(tinystore.WithoutSystem()), false}} {
		store := ways.Must(tinystore.Open(ctx, t.TempDir(), tinystore.Library(library), way.opt))
		ways.Must(struct{}{}, metrics.Flush(ctx, store))
		query := metrics.Query{Name: "tinystore_system_memory_bytes", Since: time.Hour}
		memory := ways.Must(metrics.Latest(ctx, store, query))
		if (len(memory) == 1) != way.measured {
			t.Fatalf("tinystore_system_memory_bytes with the machine %v: %+v", way.measured, memory)
		}
		_ = store.Close()
	}
}
