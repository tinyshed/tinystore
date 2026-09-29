package wire

import "unicode/utf8"

// The records engine's methods, docs/wire.md#records. Records are one log of
// the store's, whose calls name their streams, so none opens a handle.
const (
	RecordsAppend  Method = 0x0501
	RecordsRead    Method = 0x0502
	RecordsFollow  Method = 0x0503
	RecordsLines   Method = 0x0504
	RecordsDamaged Method = 0x0505
	RecordsDrop    Method = 0x0506
)

// Record is one log line or event. Context and Attrs keep their order and
// repeated keys.
type Record struct {
	// At is unix nanoseconds.
	At     int64
	Stream string
	Name   string
	// Level is slog's: -4 debug, 0 info, 4 warn, 8 error. Nil is absent.
	Level *int64
	// Body is absent when nil, which an empty body is not.
	Body *string
	// TraceID is 16 bytes and SpanID 8; empty is none.
	TraceID []byte
	SpanID  []byte
	Context []RecordField
	Attrs   []RecordField
}

// RecordField is a key and its value as JSON, spelled as it was given. Fields
// travel as one array, a key and then its value, so that keys may repeat.
type RecordField struct {
	Key   string
	Value string
}

func (r Record) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Int(1, r.At)
	m.Key(2)
	m.SetBuf(AppendText(m.Buf(), r.Stream))
	m.Key(3)
	m.SetBuf(AppendText(m.Buf(), r.Name))
	if r.Level != nil {
		m.Int(4, *r.Level)
	}
	if r.Body != nil {
		m.Key(5)
		m.SetBuf(AppendText(m.Buf(), *r.Body))
	}
	if r.TraceID != nil {
		m.Bin(6, r.TraceID)
	}
	if r.SpanID != nil {
		m.Bin(7, r.SpanID)
	}
	appendFields(&m, 8, r.Context)
	appendFields(&m, 9, r.Attrs)
	return m.End()
}

// appendFields writes fields as one array of keys and values, or nothing for
// none
//
//	user=42 tags=["a"] → ["user", "42", "tags", "[\"a\"]"]
func appendFields(m *Map, key uint64, fields []RecordField) {
	if len(fields) == 0 {
		return
	}
	m.Key(key)
	buf := AppendArray(m.Buf(), 2*len(fields))
	for _, field := range fields {
		buf = AppendText(AppendText(buf, field.Key), field.Value)
	}
	m.SetBuf(buf)
}

func (r *Record) Decode(body []byte) error {
	d := NewDecoder(body)
	r.decode(&d)
	return d.End()
}

func (r *Record) decode(d *Decoder) {
	for key := range d.Fields() {
		switch key {
		case 1:
			r.At = d.Int()
		case 2:
			r.Stream = d.Text()
		case 3:
			r.Name = d.Text()
		case 4:
			level := d.Int()
			r.Level = &level
		case 5:
			body := d.Text()
			r.Body = &body
		case 6:
			r.TraceID = clone(d.Bin())
		case 7:
			r.SpanID = clone(d.Bin())
		case 8:
			r.Context = d.recordFields()
		case 9:
			r.Attrs = d.recordFields()
		}
	}
}

// recordFields reads an array of keys and values, which holds pairs
func (d *Decoder) recordFields() []RecordField {
	var texts []string
	for range d.Items() {
		texts = append(texts, d.Text())
	}
	if len(texts)%2 == 1 {
		d.fail("fields of %d keys and values, which pair no key with its value", len(texts))
		return nil
	}
	fields := make([]RecordField, 0, len(texts)/2)
	for i := 0; i < len(texts); i += 2 {
		fields = append(fields, RecordField{Key: texts[i], Value: texts[i+1]})
	}
	return fields
}

// AppendText writes text as a str, or as a bin when its bytes are not UTF-8,
// as a program's output may be.
func AppendText(dst []byte, text string) []byte {
	if utf8.ValidString(text) {
		return AppendStr(dst, text)
	}
	return AppendBin(dst, []byte(text))
}

// Text reads a str, or a bin whose bytes are text that is not UTF-8.
func (d *Decoder) Text() string {
	if d.Type() == TypeBin {
		return string(d.Bin())
	}
	return d.Str()
}

// RecordsBatch is records.append's request: records one transaction writes,
// all or none.
type RecordsBatch struct {
	Records []Record
}

func (b RecordsBatch) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Key(1)
	buf := AppendArray(m.Buf(), len(b.Records))
	for _, record := range b.Records {
		buf = record.Append(buf)
	}
	m.SetBuf(buf)
	return m.End()
}

func (b *RecordsBatch) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		if key != 1 {
			continue
		}
		for range d.Items() {
			var record Record
			record.decode(&d)
			b.Records = append(b.Records, record)
		}
	}
	return d.End()
}

// RecordsQuery is records.read's request: the records in [From, To) that
// meet every condition given.
type RecordsQuery struct {
	// From and To are unix nanoseconds, zero for an open end.
	From, To int64
	// Streams and Names select records by them; none is every one.
	Streams  []string
	Names    []string
	MinLevel *int64
	TraceID  []byte
	Attrs    []RecordField
	Context  []RecordField
	Newest   bool
	Limit    uint64
	// Budget narrows the server's.
	Budget RecordsBudget
}

// RecordsBudget bounds one read: the blocks it opens, the bytes it fetches
// and the records it decodes.
type RecordsBudget struct {
	Blocks, Bytes, Decoded uint64
}

func (q RecordsQuery) Append(dst []byte) []byte {
	m := BeginMap(dst)
	optionalInt(&m, 1, q.From)
	optionalInt(&m, 2, q.To)
	appendTexts(&m, 3, q.Streams)
	appendTexts(&m, 4, q.Names)
	if q.MinLevel != nil {
		m.Int(5, *q.MinLevel)
	}
	if q.TraceID != nil {
		m.Bin(6, q.TraceID)
	}
	appendFields(&m, 7, q.Attrs)
	appendFields(&m, 8, q.Context)
	if q.Newest {
		m.Bool(9, true)
	}
	optionalUint(&m, 10, q.Limit)
	optionalUint(&m, 11, q.Budget.Blocks)
	optionalUint(&m, 12, q.Budget.Bytes)
	optionalUint(&m, 13, q.Budget.Decoded)
	return m.End()
}

func appendTexts(m *Map, key uint64, texts []string) {
	if len(texts) == 0 {
		return
	}
	m.Key(key)
	buf := AppendArray(m.Buf(), len(texts))
	for _, text := range texts {
		buf = AppendText(buf, text)
	}
	m.SetBuf(buf)
}

func (q *RecordsQuery) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		if key <= 6 {
			q.decodeHead(&d, key)
			continue
		}
		q.decodeTail(&d, key)
	}
	return d.End()
}

func (q *RecordsQuery) decodeHead(d *Decoder, key uint64) {
	switch key {
	case 1:
		q.From = d.Int()
	case 2:
		q.To = d.Int()
	case 3:
		q.Streams = d.texts()
	case 4:
		q.Names = d.texts()
	case 5:
		level := d.Int()
		q.MinLevel = &level
	case 6:
		q.TraceID = clone(d.Bin())
	}
}

func (q *RecordsQuery) decodeTail(d *Decoder, key uint64) {
	switch key {
	case 7:
		q.Attrs = d.recordFields()
	case 8:
		q.Context = d.recordFields()
	case 9:
		q.Newest = d.Bool()
	case 10:
		q.Limit = d.Uint()
	case 11:
		q.Budget.Blocks = d.Uint()
	case 12:
		q.Budget.Bytes = d.Uint()
	case 13:
		q.Budget.Decoded = d.Uint()
	}
}

func (d *Decoder) texts() []string {
	var texts []string
	for range d.Items() {
		texts = append(texts, d.Text())
	}
	return texts
}

// RecordsPage ends a read.
type RecordsPage struct {
	// More says the limit or the budget ended the page before the range did.
	More bool
	// From and To are the range the next page reads: the query's own with
	// one end moved past this page.
	From, To int64
}

func (p RecordsPage) Append(dst []byte) []byte {
	m := BeginMap(dst)
	if p.More {
		m.Bool(1, true)
	}
	optionalInt(&m, 2, p.From)
	optionalInt(&m, 3, p.To)
	return m.End()
}

func (p *RecordsPage) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			p.More = d.Bool()
		case 2:
			p.From = d.Int()
		case 3:
			p.To = d.Int()
		}
	}
	return d.End()
}

// RecordsCursor is a place in the sealed segments, in the order they were
// sealed. It is follow's request, with the records it asks for at most, and its
// trailer: the place the next follow begins at, with the segments retention
// removed before the cursor reached them.
type RecordsCursor struct {
	Segment int64
	Row     int64
	Limit   uint64
	Expired uint64
}

func (c RecordsCursor) Append(dst []byte) []byte {
	m := BeginMap(dst)
	optionalInt(&m, 1, c.Segment)
	optionalInt(&m, 2, c.Row)
	optionalUint(&m, 3, c.Limit)
	optionalUint(&m, 4, c.Expired)
	return m.End()
}

func (c *RecordsCursor) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			c.Segment = d.Int()
		case 2:
			c.Row = d.Int()
		case 3:
			c.Limit = d.Uint()
		case 4:
			c.Expired = d.Uint()
		}
	}
	return d.End()
}

// RecordsStream is records.lines' request: the stream another program's
// output, uploaded after it, becomes records of.
type RecordsStream struct {
	Stream string
}

func (l RecordsStream) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Key(1)
	m.SetBuf(AppendText(m.Buf(), l.Stream))
	return m.End()
}

func (l *RecordsStream) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		if key == 1 {
			l.Stream = d.Text()
		}
	}
	return d.End()
}

// RecordsDamage is a row that no longer reads: a sealed segment, dropped
// whole, or a head row, the other being zero.
type RecordsDamage struct {
	Stream  string
	Segment int64
	HeadRow int64
	// From and To are the times it held, unix nanoseconds.
	From, To int64
	// Reason is the invariant its bytes broke.
	Reason string
}

func (g RecordsDamage) Append(dst []byte) []byte {
	m := BeginMap(dst)
	g.appendFields(&m)
	return m.End()
}

func (g RecordsDamage) appendFields(m *Map) {
	m.Key(1)
	m.SetBuf(AppendText(m.Buf(), g.Stream))
	optionalInt(m, 2, g.Segment)
	optionalInt(m, 3, g.HeadRow)
	optionalInt(m, 4, g.From)
	optionalInt(m, 5, g.To)
	optionalStr(m, 6, g.Reason)
}

func (g *RecordsDamage) Decode(body []byte) error {
	d := NewDecoder(body)
	g.decode(&d)
	return d.End()
}

func (g *RecordsDamage) decode(d *Decoder) {
	for key := range d.Fields() {
		switch key {
		case 1:
			g.Stream = d.Text()
		case 2:
			g.Segment = d.Int()
		case 3:
			g.HeadRow = d.Int()
		case 4:
			g.From = d.Int()
		case 5:
			g.To = d.Int()
		case 6:
			g.Reason = d.Str()
		}
	}
}

// RecordsDamages answers records.damaged: the rows the server's records have
// met that no longer read, in the order it met them.
type RecordsDamages struct {
	Damages []RecordsDamage
}

func (l RecordsDamages) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Key(1)
	buf := AppendArray(m.Buf(), len(l.Damages))
	for _, damage := range l.Damages {
		fields := BeginMap(buf)
		damage.appendFields(&fields)
		buf = fields.End()
	}
	m.SetBuf(buf)
	return m.End()
}

func (l *RecordsDamages) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		if key != 1 {
			continue
		}
		for range d.Items() {
			var damage RecordsDamage
			damage.decode(&d)
			l.Damages = append(l.Damages, damage)
		}
	}
	return d.End()
}
