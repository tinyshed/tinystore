package wire

import (
	"encoding/binary"
	"math"
)

// The metrics engine's methods, docs/wire.md#metrics. Metrics are one store of
// series, named by their labels, so none opens a handle.
const (
	MetricsIngest    Method = 0x0601
	MetricsRead      Method = 0x0602
	MetricsAggregate Method = 0x0603
	MetricsDrop      Method = 0x0604
	MetricsExplain   Method = 0x0605
	MetricsLatest    Method = 0x0606
	MetricsDescribe  Method = 0x0607
	MetricsDescribed Method = 0x0608
)

// MetricsSeries is a series and samples of it: an item of ingest's request and
// of read's download. Times are unix milliseconds, one for each value, whose
// bits are the data.
type MetricsSeries struct {
	Labels map[string]string
	Kind   string // "gauge" or "counter"
	Times  []int64
	Values []float64
}

func (s MetricsSeries) Append(dst []byte) []byte {
	m := BeginMap(dst)
	s.appendFields(&m)
	return m.End()
}

func (s MetricsSeries) appendFields(m *Map) {
	appendLabels(m, s.Labels)
	optionalStr(m, 2, s.Kind)
	m.Key(3)
	m.SetBuf(AppendInts(m.Buf(), s.Times))
	m.Key(4)
	m.SetBuf(AppendFloats(m.Buf(), s.Values))
}

func appendLabels(m *Map, labels map[string]string) {
	m.Key(1)
	m.SetBuf(appendNames(m.Buf(), labels))
}

func (s *MetricsSeries) Decode(body []byte) error {
	d := NewDecoder(body)
	s.decode(&d)
	return d.End()
}

func (s *MetricsSeries) decode(d *Decoder) {
	for key := range d.Fields() {
		switch key {
		case 1:
			s.Labels = d.nameMap()
		case 2:
			s.Kind = d.Str()
		case 3:
			s.Times = d.Ints()
		case 4:
			s.Values = d.Floats()
		}
	}
	if len(s.Times) != len(s.Values) {
		d.fail("%d times for %d values", len(s.Times), len(s.Values))
	}
}

// AppendInts writes a column of integers as a bin, each eight bytes
// little-endian:
//
//	[1, -1] → c4 10 01 00 00 00 00 00 00 00 ff ff ff ff ff ff ff ff
func AppendInts(dst []byte, column []int64) []byte {
	dst = appendLength(dst, 8*len(column), 0, -1, mpBin8, mpBin16, mpBin32)
	for _, v := range column {
		dst = binary.LittleEndian.AppendUint64(dst, uint64(v)) //nolint:gosec // the signed bits
	}
	return dst
}

// AppendFloats writes a column of floats as a bin, each its eight bytes of
// bits little-endian, so that a NaN's payload and -0 come back as they went.
func AppendFloats(dst []byte, column []float64) []byte {
	dst = appendLength(dst, 8*len(column), 0, -1, mpBin8, mpBin16, mpBin32)
	for _, v := range column {
		dst = binary.LittleEndian.AppendUint64(dst, math.Float64bits(v))
	}
	return dst
}

// Ints reads a column of integers AppendInts wrote.
func (d *Decoder) Ints() []int64 {
	column := d.column()
	ints := make([]int64, len(column)/8)
	for i := range ints {
		ints[i] = int64(binary.LittleEndian.Uint64(column[8*i:])) //nolint:gosec // the signed bits
	}
	return ints
}

// Floats reads a column of floats AppendFloats wrote, bit for bit.
func (d *Decoder) Floats() []float64 {
	column := d.column()
	floats := make([]float64, len(column)/8)
	for i := range floats {
		floats[i] = math.Float64frombits(binary.LittleEndian.Uint64(column[8*i:]))
	}
	return floats
}

// column reads a bin of eight-byte values
func (d *Decoder) column() []byte {
	column := d.Bin()
	if len(column)%8 != 0 {
		d.fail("a column of %d bytes, which holds no whole number of eight-byte values", len(column))
		return nil
	}
	return column
}

// MetricsBatch is metrics.ingest's request: series and their samples one call
// stores, all or none.
type MetricsBatch struct {
	Series []MetricsSeries
}

func (b MetricsBatch) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Key(1)
	buf := AppendArray(m.Buf(), len(b.Series))
	for _, series := range b.Series {
		buf = series.Append(buf)
	}
	m.SetBuf(buf)
	return m.End()
}

func (b *MetricsBatch) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		if key != 1 {
			continue
		}
		for range d.Items() {
			var series MetricsSeries
			series.decode(&d)
			b.Series = append(b.Series, series)
		}
	}
	return d.End()
}

// MetricsRange is read's and aggregate's request: the series every matcher
// names exactly, in [From, To) of unix milliseconds, within limits that may
// only narrow the server's. Aggregate adds its buckets' width in milliseconds,
// its operation, and how far before From an increase, a rate or a delta looks
// for its first step, Width when zero.
type MetricsRange struct {
	Matchers map[string]string
	From, To int64
	Limits   MetricsLimits
	Width    int64
	Op       string // count, sum, min, max, avg, increase, rate or delta
	Where    []MetricsCondition
	// By and Without group an aggregate's series; an empty one, not nil, is
	// sent and groups by no label.
	By, Without []string
	Lookback    int64
}

// MetricsCondition is what a label's value must be beyond equality: one_of
// or none_of its values, or the prefix that is its one value.
type MetricsCondition struct {
	Label  string
	Kind   string
	Values []string
}

func (c MetricsCondition) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Str(1, c.Label)
	m.Str(2, c.Kind)
	m.Key(3)
	m.SetBuf(appendStrs(m.Buf(), c.Values))
	return m.End()
}

func (c *MetricsCondition) decode(d *Decoder) {
	for key := range d.Fields() {
		switch key {
		case 1:
			c.Label = d.Str()
		case 2:
			c.Kind = d.Str()
		case 3:
			c.Values = d.Strs()
		}
	}
}

// MetricsLimits bound one read or aggregate: the series it matches, the blocks
// it decodes, the bytes it fetches, the samples it decodes and the samples or
// buckets it answers. Zero is the server's.
type MetricsLimits struct {
	Series, Blocks, PayloadBytes, DecodedSamples, OutputSamples uint64
}

func (r MetricsRange) Append(dst []byte) []byte {
	m := BeginMap(dst)
	appendLabels(&m, r.Matchers)
	m.Int(2, r.From)
	m.Int(3, r.To)
	optionalUint(&m, 4, r.Limits.Series)
	optionalUint(&m, 5, r.Limits.Blocks)
	optionalUint(&m, 6, r.Limits.PayloadBytes)
	optionalUint(&m, 7, r.Limits.DecodedSamples)
	optionalUint(&m, 8, r.Limits.OutputSamples)
	optionalInt(&m, 9, r.Width)
	optionalStr(&m, 10, r.Op)
	if len(r.Where) > 0 {
		m.Key(11)
		buf := AppendArray(m.Buf(), len(r.Where))
		for _, condition := range r.Where {
			buf = condition.Append(buf)
		}
		m.SetBuf(buf)
	}
	if r.By != nil {
		m.Key(12)
		m.SetBuf(appendStrs(m.Buf(), r.By))
	}
	if r.Without != nil {
		m.Key(13)
		m.SetBuf(appendStrs(m.Buf(), r.Without))
	}
	optionalInt(&m, 14, r.Lookback)
	return m.End()
}

func (r *MetricsRange) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			r.Matchers = d.nameMap()
		case 2:
			r.From = d.Int()
		case 3:
			r.To = d.Int()
		case 9:
			r.Width = d.Duration()
		case 10:
			r.Op = d.Str()
		case 11:
			for range d.Items() {
				var condition MetricsCondition
				condition.decode(&d)
				r.Where = append(r.Where, condition)
			}
		case 12:
			r.By = append([]string{}, d.Strs()...)
		case 13:
			r.Without = append([]string{}, d.Strs()...)
		case 14:
			r.Lookback = d.Duration()
		default:
			r.decodeLimit(&d, key)
		}
	}
	return d.End()
}

func (r *MetricsRange) decodeLimit(d *Decoder, key uint64) {
	switch key {
	case 4:
		r.Limits.Series = d.Uint()
	case 5:
		r.Limits.Blocks = d.Uint()
	case 6:
		r.Limits.PayloadBytes = d.Uint()
	case 7:
		r.Limits.DecodedSamples = d.Uint()
	case 8:
		r.Limits.OutputSamples = d.Uint()
	}
}

// MetricsBuckets is an item of aggregate's download: a series and buckets of
// it, the edges it was asked for.
type MetricsBuckets struct {
	Labels  map[string]string
	Kind    string
	Buckets []MetricsBucket
}

// MetricsBucket is one bucket of an aggregate.
type MetricsBucket struct {
	// From and To are the bucket's edges in unix milliseconds.
	From, To int64
	// Count is the samples it counted and Resets the resets among them.
	Count, Resets int64
	Value         float64
	// Overflow says the value overflowed a float.
	Overflow bool
	// Partial says retention cut the bucket.
	Partial bool
	// Lookback says its first step started from a sample before the range.
	Lookback bool
}

// a bucket's flags, a byte each
const (
	bucketOverflow = 1
	bucketPartial  = 2
	bucketLookback = 4
)

func (b MetricsBuckets) Append(dst []byte) []byte {
	m := BeginMap(dst)
	appendLabels(&m, b.Labels)
	optionalStr(&m, 2, b.Kind)
	n := len(b.Buckets)
	from, to, count, resets := make([]int64, n), make([]int64, n), make([]int64, n), make([]int64, n)
	values, flags := make([]float64, n), make([]byte, n)
	for i, bucket := range b.Buckets {
		from[i], to[i], values[i] = bucket.From, bucket.To, bucket.Value
		count[i], resets[i] = bucket.Count, bucket.Resets
		if bucket.Overflow {
			flags[i] |= bucketOverflow
		}
		if bucket.Partial {
			flags[i] |= bucketPartial
		}
		if bucket.Lookback {
			flags[i] |= bucketLookback
		}
	}
	for key, column := range [][]int64{from, to, count, resets} {
		m.Key(uint64(key) + 3)
		m.SetBuf(AppendInts(m.Buf(), column))
	}
	m.Key(7)
	m.SetBuf(AppendFloats(m.Buf(), values))
	m.Bin(8, flags)
	return m.End()
}

func (b *MetricsBuckets) Decode(body []byte) error {
	d := NewDecoder(body)
	var ints [4][]int64
	var values []float64
	var flags []byte
	for key := range d.Fields() {
		switch key {
		case 1:
			b.Labels = d.nameMap()
		case 2:
			b.Kind = d.Str()
		case 3, 4, 5, 6:
			ints[key-3] = d.Ints()
		case 7:
			values = d.Floats()
		case 8:
			flags = clone(d.Bin())
		}
	}
	n := len(values)
	if len(ints[0]) != n || len(ints[1]) != n || len(ints[2]) != n || len(ints[3]) != n || len(flags) != n {
		d.fail("columns of buckets of different lengths")
	}
	if d.err == nil && n > 0 {
		b.Buckets = make([]MetricsBucket, n)
		for i := range b.Buckets {
			b.Buckets[i] = MetricsBucket{
				From: ints[0][i], To: ints[1][i], Count: ints[2][i], Resets: ints[3][i], Value: values[i],
				Overflow: flags[i]&bucketOverflow != 0, Partial: flags[i]&bucketPartial != 0,
				Lookback: flags[i]&bucketLookback != 0,
			}
		}
	}
	return d.End()
}

// MetricsDescription is describe's request and described's answer: what a
// metric's name means, its unit and a line of help, both empty when it has
// none. Described asks with the name alone.
type MetricsDescription struct {
	Name, Unit, Help string
}

func (d MetricsDescription) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Str(1, d.Name)
	optionalStr(&m, 2, d.Unit)
	optionalStr(&m, 3, d.Help)
	return m.End()
}

func (d *MetricsDescription) Decode(body []byte) error {
	dec := NewDecoder(body)
	for key := range dec.Fields() {
		switch key {
		case 1:
			d.Name = dec.Str()
		case 2:
			d.Unit = dec.Str()
		case 3:
			d.Help = dec.Str()
		}
	}
	return dec.End()
}

// MetricsLabels is drop's request: the labels of the one series it removes.
type MetricsLabels struct {
	Labels map[string]string
}

func (l MetricsLabels) Append(dst []byte) []byte {
	m := BeginMap(dst)
	appendLabels(&m, l.Labels)
	return m.End()
}

func (l *MetricsLabels) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		if key == 1 {
			l.Labels = d.nameMap()
		}
	}
	return d.End()
}

// MetricsPlan is metrics.explain's answer: what the read, or the aggregate
// when the range names an operation, would spend, each beside its limit, and
// the limit it would stop at, an error of code limit naming it.
type MetricsPlan struct {
	Series, Blocks, Summarized, Bytes, Decoded uint64
	Limits                                     MetricsLimits
	Stops                                      *Error
}

func (p MetricsPlan) Append(dst []byte) []byte {
	m := BeginMap(dst)
	optionalUint(&m, 1, p.Series)
	optionalUint(&m, 2, p.Blocks)
	optionalUint(&m, 3, p.Summarized)
	optionalUint(&m, 4, p.Bytes)
	optionalUint(&m, 5, p.Decoded)
	optionalUint(&m, 6, p.Limits.Series)
	optionalUint(&m, 7, p.Limits.Blocks)
	optionalUint(&m, 8, p.Limits.PayloadBytes)
	optionalUint(&m, 9, p.Limits.DecodedSamples)
	optionalUint(&m, 10, p.Limits.OutputSamples)
	if p.Stops != nil {
		m.Key(11)
		m.SetBuf(p.Stops.Append(m.Buf()))
	}
	return m.End()
}

func (p *MetricsPlan) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			p.Series = d.Uint()
		case 2:
			p.Blocks = d.Uint()
		case 3:
			p.Summarized = d.Uint()
		case 4:
			p.Bytes = d.Uint()
		case 5:
			p.Decoded = d.Uint()
		case 11:
			p.Stops = &Error{}
			p.Stops.decode(&d)
		default:
			p.decodeLimit(&d, key)
		}
	}
	return d.End()
}

func (p *MetricsPlan) decodeLimit(d *Decoder, key uint64) {
	switch key {
	case 6:
		p.Limits.Series = d.Uint()
	case 7:
		p.Limits.Blocks = d.Uint()
	case 8:
		p.Limits.PayloadBytes = d.Uint()
	case 9:
		p.Limits.DecodedSamples = d.Uint()
	case 10:
		p.Limits.OutputSamples = d.Uint()
	}
}

// MetricsDropped answers drop: whether the series was there, and the groups
// removed without the payload rows their directory no longer names.
type MetricsDropped struct {
	Found            bool
	UnreadableGroups uint64
}

func (r MetricsDropped) Append(dst []byte) []byte {
	m := BeginMap(dst)
	if r.Found {
		m.Bool(1, true)
	}
	optionalUint(&m, 2, r.UnreadableGroups)
	return m.End()
}

func (r *MetricsDropped) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			r.Found = d.Bool()
		case 2:
			r.UnreadableGroups = d.Uint()
		}
	}
	return d.End()
}
