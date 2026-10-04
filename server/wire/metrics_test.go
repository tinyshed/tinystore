package wire_test

import (
	"errors"
	"math"
	"reflect"
	"testing"

	"github.com/tinyshed/tinystore/server/wire"
)

// A value comes back with the bits it went in with.
func TestMetricsMessagesReadBackAsTheyWereWritten(t *testing.T) {
	cpu := map[string]string{"__name__": "cpu", "host": "web-1"}
	nan := math.Float64frombits(0x7ff8000000000001)
	series := wire.MetricsSeries{
		Labels: cpu, Kind: "gauge", Times: []int64{-1, 0, 1 << 40},
		Values: []float64{math.Copysign(0, -1), nan, math.Inf(1)},
	}
	messages := []struct {
		written interface{ Append([]byte) []byte }
		read    interface{ Decode([]byte) error }
	}{
		{wire.MetricsBatch{Series: []wire.MetricsSeries{series, {
			Labels: cpu, Kind: "counter",
			Times: []int64{}, Values: []float64{},
		}}}, &wire.MetricsBatch{}},
		{wire.MetricsRange{Matchers: cpu, From: 5, To: 9, Width: 60_000, Op: "sum", Lookback: 120_000, Limits: wire.MetricsLimits{
			Series: 1, Blocks: 2, PayloadBytes: 3, DecodedSamples: 4, OutputSamples: 5,
		}}, &wire.MetricsRange{}},
		{wire.MetricsBuckets{Labels: cpu, Kind: "counter", Buckets: []wire.MetricsBucket{
			{From: 0, To: 60_000, Count: 3, Resets: 1, Value: 1.5, Partial: true, Lookback: true},
			{From: 60_000, To: 120_000, Count: 1, Value: math.Inf(1), Overflow: true},
		}}, &wire.MetricsBuckets{}},
		{wire.MetricsLabels{Labels: cpu}, &wire.MetricsLabels{}},
		{wire.MetricsDropped{Found: true, UnreadableGroups: 2}, &wire.MetricsDropped{}},
	}
	for _, m := range messages {
		if err := m.read.Decode(m.written.Append(nil)); err != nil {
			t.Errorf("%T: %v", m.written, err)
			continue
		}
		got := reflect.ValueOf(m.read).Elem().Interface()
		if written, ok := m.written.(wire.MetricsBatch); ok {
			if !sameSeries(got.(wire.MetricsBatch).Series[0], written.Series[0]) {
				t.Errorf("a series read as %+v", got)
			}
			continue
		}
		if !reflect.DeepEqual(got, m.written) {
			t.Errorf("%T read as %+v", m.written, got)
		}
	}
}

// sameSeries compares values by their bits, which DeepEqual would not for a
// NaN
func sameSeries(a, b wire.MetricsSeries) bool {
	if !reflect.DeepEqual(a.Labels, b.Labels) || a.Kind != b.Kind || !reflect.DeepEqual(a.Times, b.Times) ||
		len(a.Values) != len(b.Values) {
		return false
	}
	for i := range a.Values {
		if math.Float64bits(a.Values[i]) != math.Float64bits(b.Values[i]) {
			return false
		}
	}
	return true
}

// a column is whole eight-byte values, and a series has a time for each value
func TestAMetricsColumnHoldsWholeValues(t *testing.T) {
	for _, refused := range []wire.MetricsSeries{
		{Labels: map[string]string{"__name__": "x"}, Times: []int64{1, 2}, Values: []float64{1}},
	} {
		if err := (&wire.MetricsSeries{}).Decode(refused.Append(nil)); !errors.Is(err, wire.ErrMessage) {
			t.Errorf("%+v taken: %v", refused, err)
		}
	}
	m := wire.BeginMap(nil)
	m.Bin(3, make([]byte, 7))
	if err := (&wire.MetricsSeries{}).Decode(m.End()); !errors.Is(err, wire.ErrMessage) {
		t.Errorf("a column of seven bytes: %v", err)
	}
}
